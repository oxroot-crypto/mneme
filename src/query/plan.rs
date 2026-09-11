//! 查询计划器(设计 06 §2)。
//!
//! 把过滤 AST 编译成"每查询一份"的执行方案:块位图(zone map/bloom 下推)
//! 只减少行级求值;候选位图由向量与 BM25 两通道共享(过滤先行)。残余谓词
//! 仍以三值求值为准,故候选集合与逐行求值全等(下推只减少工作量)。

use crate::core::bitset::BitSet;
use crate::core::types::NsId;
use crate::memory::analysis::ZONE_BLOCK_ROWS;
use crate::memory::pred::{self, EvalCtx, Expr};
use crate::memory::table::ReaderView;

use super::zmap;

// 单测操作计数:统计残余谓词的行级求值次数(线程局部,避免测试间干扰)。
#[cfg(test)]
thread_local! {
    static ROW_EVALS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn bump_row_evals() {
    ROW_EVALS.with(|evals| evals.set(evals.get() + 1));
}

/// 一次查询的执行计划。
pub(crate) struct Plan {
    /// 过滤后候选槽位(行级残余谓词已求值)。
    pub(crate) candidates: Vec<u32>,
    /// 候选槽位位图(BM25 通道共享)。
    pub(crate) bits: BitSet,
    /// 选择性 = 候选数 / 命名空间活行数(无活行时为 0)。
    pub(crate) selectivity: f32,
}

/// 编译过滤表达式为执行计划。
///
/// # Arguments
/// * `view` - 不可变读视图。
/// * `ns_id` - 目标命名空间。
/// * `filter` - 过滤表达式;`None` 表示全量活行。
/// * `now_ms` - 当前时刻(TTL 可见性)。
pub(crate) fn compile(view: &ReaderView, ns_id: NsId, filter: Option<&Expr>, now_ms: i64) -> Plan {
    let mask = filter.map_or_else(
        || zmap::full_mask(zmap::block_count(view)),
        |expr| zmap::block_mask(expr, view),
    );
    let mut candidates = Vec::new();
    let mut bits = BitSet::default();
    let mut alive = 0_usize;
    for (index, slot) in view.slots.iter().enumerate() {
        if view.dead.get(index) || slot.ns_id != ns_id || !slot.is_live(now_ms) {
            continue;
        }
        alive += 1;
        if !mask.get(index / ZONE_BLOCK_ROWS) {
            continue;
        }
        if let Some(expr) = filter {
            let ctx = EvalCtx {
                slot,
                access: view.access.get(&slot.rowid).copied(),
            };
            #[cfg(test)]
            bump_row_evals();
            if !pred::matches(expr, &ctx) {
                continue;
            }
        }
        // 槽位下标 ≤ u32::MAX:commit_version 经 `slot_id_for` 拒绝继续增长,
        // 下标越界即违反 FC-MEM-INV-004,故此转换可证明不会失败。
        candidates.push(u32::try_from(index).expect("槽位下标必可转入 u32(FC-MEM-INV-004)"));
        bits.set(index);
    }
    let selectivity = if alive == 0 {
        0.0
    } else {
        candidates.len() as f32 / alive as f32
    };
    Plan {
        candidates,
        bits,
        selectivity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::pred::{Expr, Val};
    use crate::memory::search::{self, CandidateQuery};
    use crate::memory::{Mneme, Record};

    /// 建库并返回 `(db, ns_id)`;前 1024 条 `importance=0.1`,随后 6 条 `0.9`,
    /// 使块 0 可被 `importance > 0.5` 整体剪除。
    fn setup() -> (Mneme, NsId) {
        let db = Mneme::in_memory(2).expect("in_memory");
        let ns = db.namespace("n");
        for index in 0..1030 {
            let importance = if index < 1024 { 0.1_f32 } else { 0.9 };
            ns.insert(
                Record::new(vec![1.0, 0.0])
                    .key(format!("k{index}"))
                    .importance(importance)
                    .metadata(crate::core::meta::json!({
                        "rank": index,
                        "kind": if index % 3 == 0 { "a" } else { "b" },
                    })),
            )
            .expect("insert");
        }
        let view = db.table.view();
        let ns_id = view
            .ns_registry
            .iter()
            .find_map(|(id, path)| (**path == *"n").then_some(*id))
            .expect("命名空间已注册");
        (db, ns_id)
    }

    /// 断言计划候选与逐行求值全等,并返回计划候选数。
    fn assert_matches_bruteforce(view: &ReaderView, ns_id: NsId, expr: &Expr) -> usize {
        let plan = compile(view, ns_id, Some(expr), 0);
        let brute = search::collect_candidates(&CandidateQuery {
            view,
            ns_id,
            filter: Some(expr),
            now_ms: 0,
        });
        assert_eq!(plan.candidates, brute, "下推不得改变候选: {expr}");
        plan.candidates.len()
    }

    /// FC-QUERY-POST-005 / FC-QUERY-CPLX-002(块级剪枝与逐行求值全等)
    #[test]
    fn plan_candidates_match_bruteforce() {
        let (db, ns_id) = setup();
        let view = db.table.view();
        for expr in [
            Expr::field("importance").gt(0.5_f32),
            Expr::field("importance").ge(0.9_f32),
            Expr::field("importance").le(0.1_f32),
            Expr::field("rank").lt(20_i64),
            Expr::field("rank").ne(5_i64),
            Expr::field("kind").eq("a"),
            Expr::Exists("rank".into()),
            Expr::IsNull("rank".into()),
            Expr::In(
                "rank".into(),
                vec![Val::Int(1), Val::Int(2), Val::Int(1025)].into_boxed_slice(),
            ),
            Expr::field("importance").gt(0.5_f32) & Expr::field("rank").lt(1028_i64),
            Expr::Not(Box::new(Expr::field("importance").gt(0.5_f32))),
            Expr::field("importance").eq(0.1_f32) | Expr::Never,
            Expr::Always,
            Expr::Never,
        ] {
            assert_matches_bruteforce(&view, ns_id, &expr);
        }
    }

    /// FC-QUERY-POST-005(块级剪枝真实生效:块 0 被整体剪除)
    #[test]
    fn block_pruning_skips_impossible_blocks() {
        let (db, ns_id) = setup();
        let view = db.table.view();
        let expr = Expr::field("importance").gt(0.5_f32);
        let count = assert_matches_bruteforce(&view, ns_id, &expr);
        assert_eq!(count, 6, "块 0(max=0.1)应被剪除,只剩后 6 条");
    }

    /// FC-QUERY-POST-005(`key` 等值经 bloom 预筛;否定即零候选)
    #[test]
    fn key_bloom_rejects_absent_key() {
        let (db, ns_id) = setup();
        let view = db.table.view();
        let count = assert_matches_bruteforce(&view, ns_id, &Expr::field("key").eq("no-such-key"));
        assert_eq!(count, 0, "bloom 否定必须直接得到空候选");
        assert_matches_bruteforce(&view, ns_id, &Expr::field("key").eq("k1028"));
    }

    /// FC-QUERY-POST-005(bloom 否定必须避免行级求值,证伪"预筛未被走到")
    #[test]
    fn key_bloom_skips_row_evaluation() {
        let (db, ns_id) = setup();
        let view = db.table.view();
        ROW_EVALS.with(|evals| evals.set(0));
        let count = assert_matches_bruteforce(&view, ns_id, &Expr::field("key").eq("no-such-key"));
        assert_eq!(count, 0);
        assert_eq!(
            ROW_EVALS.with(std::cell::Cell::get),
            0,
            "bloom 否定应整块剪除候选,不触发任何行级求值"
        );
        // 对照:命中存在的 key 时仍有行级求值,防止探针自身失效造成假绿。
        ROW_EVALS.with(|evals| evals.set(0));
        assert_matches_bruteforce(&view, ns_id, &Expr::field("key").eq("k1028"));
        assert!(ROW_EVALS.with(std::cell::Cell::get) > 0);
    }

    /// FC-QUERY-POST-005(选择性 = 候选 / 活行)
    #[test]
    fn selectivity_reflects_candidate_ratio() {
        let (db, ns_id) = setup();
        let view = db.table.view();
        let plan = compile(&view, ns_id, None, 0);
        assert_eq!(plan.candidates.len(), 1030);
        assert!((plan.selectivity - 1.0).abs() < 1e-6);
        let filtered = compile(
            &view,
            ns_id,
            Some(&Expr::field("importance").gt(0.5_f32)),
            0,
        );
        assert!((filtered.selectivity - 6.0 / 1030.0).abs() < 1e-6);
    }
}
