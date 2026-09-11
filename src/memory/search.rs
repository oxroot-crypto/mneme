//! 暴力扫描 / ANN 检索(`search.rs`)。
//!
//! **过滤先行**:先用元数据求值得到候选位图(不读向量),再分块并行打分、
//! `TopK` 归并。这是设计 03 §4 的 L1 落地。L3 起:若读视图带有覆盖槽位前缀的
//! 向量索引且前缀规模超过 `brute_force_max_rows`,前缀走 HNSW(过滤三档),
//! 未建树的尾部仍暴力扫描,二者 `TopK` 归并——语义与纯暴力统计等价(设计 05 §9)。

use crate::core::bitset::BitSet;
use crate::core::error::{MnemeError, Result};
use crate::core::heap::TopK;
use crate::core::metric::{Metric, Score};
use crate::core::simd;
use crate::core::types::{NsId, RowId, SlotId};
use crate::memory::index::{IndexSearch, VectorIndex};
use crate::memory::pred::{self, EvalCtx, Expr};
use crate::memory::table::ReaderView;

// 单测操作计数:统计扫描阶段的打分次数(线程局部,避免测试间干扰)。
#[cfg(test)]
thread_local! {
    static SCORE_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn bump_score_calls() {
    SCORE_CALLS.with(|calls| calls.set(calls.get() + 1));
}

/// 一条被打分的候选。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scored {
    /// 物理槽位。
    pub(crate) slot: SlotId,
    /// 稳定逻辑标识。
    pub(crate) rowid: RowId,
    /// 原始相似度分。
    pub(crate) score: Score,
}

/// 计算向量范数平方。
pub(crate) fn norm_sq(vector: &[f32]) -> f32 {
    simd::dot(vector, vector)
}

/// 一次检索的全部输入。
pub(crate) struct SearchParams<'a> {
    /// 不可变读视图。
    pub(crate) view: &'a ReaderView,
    /// 目标命名空间。
    pub(crate) ns_id: NsId,
    /// 查询向量。
    pub(crate) query: &'a [f32],
    /// 距离度量。
    pub(crate) metric: Metric,
    /// 返回条数。
    pub(crate) top_k: usize,
    /// ANN 探查宽度(仅前缀走索引时生效)。
    pub(crate) ef: usize,
    /// 预过滤表达式。
    pub(crate) filter: Option<&'a Expr>,
    /// 当前时刻(Unix 毫秒)。
    pub(crate) now_ms: i64,
    /// 并行分块行数。
    pub(crate) block: usize,
    /// 线程数;`0` = 自动。
    pub(crate) parallelism: usize,
    /// 段行数低于此值恒暴力扫描(设计 05 §11)。
    pub(crate) brute_force_max_rows: usize,
    /// 过滤三档:后过滤 / 放大后过滤分界。
    pub(crate) filter_post_threshold: f32,
    /// 过滤三档:放大后过滤 / 候选暴力分界。
    pub(crate) filter_brute_threshold: f32,
}

/// 分块扫描的输入(顺序与并行共用)。
struct ScanParams<'a> {
    view: &'a ReaderView,
    candidates: &'a [u32],
    query: &'a [f32],
    query_norm: f32,
    metric: Metric,
    k: usize,
    chunk: usize,
    threads: usize,
}

/// 在给定快照上做过滤 + 检索,返回按 `Metric::better` 排序的 top-k。
pub(crate) fn search(params: &SearchParams<'_>) -> Result<Vec<Scored>> {
    let query_norm = if params.metric.needs_norm() {
        norm_sq(params.query)
    } else {
        0.0
    };

    let candidates = collect_candidates(params);
    let k = params.top_k.min(candidates.len());
    if k == 0 {
        return Ok(Vec::new());
    }

    // L3:前缀走 HNSW(ANN),尾部暴力扫描(内存段),归并后统一重取分。
    if let Some(index) = params.view.index.as_ref()
        && index.node_count() > params.brute_force_max_rows
    {
        let budget = AnnBudget { query_norm, k };
        return ann_search(params, index.as_ref(), &candidates, budget);
    }

    let top = run_scan(params, &candidates, query_norm, k)?;
    Ok(rescore(params, top, query_norm))
}

/// 过滤先行:只求值元数据,得到候选槽位(不读向量)。
fn collect_candidates(params: &SearchParams<'_>) -> Vec<u32> {
    let mut candidates: Vec<u32> = Vec::new();
    for (idx, slot) in params.view.slots.iter().enumerate() {
        if params.view.dead.get(idx) || slot.ns_id != params.ns_id || !slot.is_live(params.now_ms) {
            continue;
        }
        if let Some(expr) = params.filter {
            let ctx = EvalCtx {
                slot,
                access: params.view.access.get(&slot.rowid).copied(),
            };
            if !pred::matches(expr, &ctx) {
                continue;
            }
        }
        // 槽位下标 ≤ u32::MAX:commit_version 经 `slot_id_for` 拒绝继续增长,
        // 下标越界即违反 FC-MEM-INV-004,故此转换可证明不会失败。
        candidates.push(u32::try_from(idx).expect("槽位下标必可转入 u32(FC-MEM-INV-004)"));
    }
    candidates
}

/// ANN 路径的预算参数:`search` 已算好查询范数与有效 `k`,打包传入避免参数堆叠。
struct AnnBudget {
    query_norm: f32,
    k: usize,
}

/// ANN 路径:前缀 HNSW + 尾部暴力,归并后再取分。
fn ann_search(
    params: &SearchParams<'_>,
    index: &dyn VectorIndex,
    candidates: &[u32],
    budget: AnnBudget,
) -> Result<Vec<Scored>> {
    let AnnBudget { query_norm, k } = budget;
    let indexed = index.node_count().min(params.view.slots.len());
    let (alive, filter) = prefix_bitmaps(params, indexed, candidates);
    let index_top = index.search(&IndexSearch {
        query: params.query,
        query_norm,
        ef: params.ef,
        k,
        alive: &alive,
        filter: filter.as_ref(),
        post_threshold: params.filter_post_threshold,
        brute_threshold: params.filter_brute_threshold,
    });

    let tail: Vec<u32> = candidates
        .iter()
        .copied()
        .filter(|&idx| idx as usize >= indexed)
        .collect();
    let top = if tail.is_empty() {
        index_top
    } else {
        let mut merged = run_scan(params, &tail, query_norm, k)?;
        merged.merge(index_top);
        merged
    };
    Ok(rescore(params, top, query_norm))
}

/// 构造索引前缀的 `alive` 位图与候选过滤位图(均按全局槽位)。
fn prefix_bitmaps(
    params: &SearchParams<'_>,
    indexed: usize,
    candidates: &[u32],
) -> (BitSet, Option<BitSet>) {
    let mut alive = BitSet::default();
    for idx in 0..indexed {
        let slot = &params.view.slots[idx];
        if !params.view.dead.get(idx) && slot.ns_id == params.ns_id && slot.is_live(params.now_ms) {
            alive.set(idx);
        }
    }
    let filter = params.filter.map(|_| {
        let mut bits = BitSet::default();
        for &idx in candidates {
            if (idx as usize) < indexed {
                bits.set(idx as usize);
            }
        }
        bits
    });
    (alive, filter)
}

/// 决定分块大小与线程数并执行一次暴力扫描。
fn run_scan(
    params: &SearchParams<'_>,
    candidates: &[u32],
    query_norm: f32,
    k: usize,
) -> Result<TopK<(RowId, SlotId)>> {
    let (chunk, threads) = plan_scan(params);
    let scan = ScanParams {
        view: params.view,
        candidates,
        query: params.query,
        query_norm,
        metric: params.metric,
        k,
        chunk,
        threads,
    };
    if candidates.len() <= chunk || threads <= 1 {
        Ok(scan_sequential(&scan))
    } else {
        // 工作线程异常终止 = 打分阶段内部不变量被破坏;按 FC-GLOBAL-ERR-001 不向外
        // 传播 panic,也不静默返回偏少的结果,而是转为结构化错误(FC-MEM-INV-004 口径)。
        scan_parallel(&scan)
    }
}

/// 根据候选规模决定分块大小与线程数。
fn plan_scan(params: &SearchParams<'_>) -> (usize, usize) {
    let chunk = params.block.max(1);
    let threads = if params.parallelism == 0 {
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    } else {
        params.parallelism
    };
    (chunk, threads)
}

/// 重新取分:TopK 只返回载荷,分数在候选集内 O(1) 重算。
fn rescore(params: &SearchParams<'_>, top: TopK<(RowId, SlotId)>, query_norm: f32) -> Vec<Scored> {
    top.into_sorted_vec()
        .into_iter()
        .map(|(rowid, slot)| {
            let slot_data = &params.view.slots[slot.get() as usize];
            let score = params.metric.score(
                params.query,
                &slot_data.vector,
                query_norm,
                slot_data.norm_sq,
            );
            Scored { slot, rowid, score }
        })
        .collect()
}

fn scan_sequential(params: &ScanParams<'_>) -> TopK<(RowId, SlotId)> {
    let mut top = TopK::new(params.k, params.metric);
    for &idx in params.candidates {
        let (rowid, slot, score) = score_slot(params, idx);
        top.push(score, (rowid, slot));
    }
    top
}

/// 对候选 `idx` 打分;单测时累计操作计数(FC-MEM-CPLX-001)。
fn score_slot(params: &ScanParams<'_>, idx: u32) -> (RowId, SlotId, Score) {
    let slot = SlotId::new(idx);
    let slot_data = &params.view.slots[idx as usize];
    let score = params.metric.score(
        params.query,
        &slot_data.vector,
        params.query_norm,
        slot_data.norm_sq,
    );
    #[cfg(test)]
    bump_score_calls();
    (slot_data.rowid, slot, score)
}

fn scan_parallel(params: &ScanParams<'_>) -> Result<TopK<(RowId, SlotId)>> {
    let chunk = params
        .chunk
        .max(params.candidates.len().div_ceil(params.threads));
    let partials = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for part in params.candidates.chunks(chunk) {
            handles.push(scope.spawn(move || {
                let mut local = TopK::new(params.k, params.metric);
                for &idx in part {
                    let (rowid, slot, score) = score_slot(params, idx);
                    local.push(score, (rowid, slot));
                }
                local
            }));
        }
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(local) => Ok(local),
                // 工作线程 panic:打分阶段的内部不变量被破坏,绝不静默吞掉
                // (静默吞掉会返回偏少的结果,违反「拒绝静默失败」)。
                Err(_) => Err(MnemeError::Inconsistent {
                    reason: "并行扫描工作线程异常终止",
                }),
            })
            .collect::<Result<Vec<_>>>()
    })?;

    let mut top = TopK::new(params.k, params.metric);
    for partial in partials {
        top.merge(partial);
    }
    Ok(top)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_sq_of_unit_basis_is_one() {
        assert!((norm_sq(&[1.0, 0.0, 0.0]) - 1.0).abs() < 1e-6);
    }

    /// FC-MEM-CPLX-001(操作计数:扫描打分次数 = 候选数,无隐藏全扫)
    #[test]
    fn scan_touches_each_candidate_once() {
        let db = crate::memory::Mneme::in_memory(2).expect("in_memory");
        let ns = db.namespace("n");
        for index in 0..8 {
            ns.insert(crate::memory::Record::new(vec![1.0, 0.0]).key(format!("k{index}")))
                .expect("insert");
        }
        SCORE_CALLS.with(|calls| calls.set(0));
        let hits = ns
            .search()
            .vector(&[1.0, 0.0])
            .top_k(4)
            .execute()
            .expect("search");
        assert_eq!(hits.len(), 4);
        let calls = SCORE_CALLS.with(std::cell::Cell::get);
        assert_eq!(calls, 8, "扫描打分次数必须等于候选数(线性,无隐藏全扫)");
    }

    /// FC-INDEX-POST-009(分派):段行数超过 `brute_force_max_rows` 且视图带索引时,
    /// 查询必须走索引搜索,不得回退为全量暴力重扫——否则恒暴力退化解无人察觉。
    #[test]
    fn search_dispatches_to_index_when_prefix_exceeds_brute_threshold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::memory::Builder::default()
            .path(dir.path())
            .dimension(4)
            .tuning(crate::core::options::Tuning {
                brute_force_max_rows: 8,
                ..crate::core::options::Tuning::default()
            })
            .build()
            .expect("build");
        let ns = db.namespace("n");
        let batch: Vec<crate::memory::Record> = (0..64)
            .map(|row| crate::memory::Record::new(vec![row as f32, 1.0, 0.0, 0.0]))
            .collect();
        ns.insert_batch(batch).expect("insert_batch");
        db.flush().expect("flush");

        SCORE_CALLS.with(|calls| calls.set(0));
        let hits = ns
            .search()
            .vector(&[1.0, 0.0, 0.0, 0.0])
            .top_k(4)
            .ef(16)
            .execute()
            .expect("search");
        assert_eq!(hits.len(), 4);
        let calls = SCORE_CALLS.with(std::cell::Cell::get);
        assert_eq!(
            calls, 0,
            "有索引且段行数超过阈值时必须走索引,不得回退暴力重扫前缀"
        );
    }

    /// FC-INDEX-POST-009(分派边界):段行数等于 `brute_force_max_rows` 时仍走暴力
    /// (契约口径为严格「超过」),且索引存在与否不改变该边界判定。
    #[test]
    fn search_bruteforces_when_prefix_does_not_exceed_threshold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::memory::Builder::default()
            .path(dir.path())
            .dimension(4)
            .tuning(crate::core::options::Tuning {
                brute_force_max_rows: 64,
                ..crate::core::options::Tuning::default()
            })
            .build()
            .expect("build");
        let ns = db.namespace("n");
        let batch: Vec<crate::memory::Record> = (0..64)
            .map(|row| crate::memory::Record::new(vec![row as f32, 1.0, 0.0, 0.0]))
            .collect();
        ns.insert_batch(batch).expect("insert_batch");
        db.flush().expect("flush");

        SCORE_CALLS.with(|calls| calls.set(0));
        let hits = ns
            .search()
            .vector(&[1.0, 0.0, 0.0, 0.0])
            .top_k(4)
            .ef(16)
            .execute()
            .expect("search");
        assert_eq!(hits.len(), 4);
        let calls = SCORE_CALLS.with(std::cell::Cell::get);
        assert_eq!(
            calls, 64,
            "行数等于阈值(未严格超过)时必须走暴力;若索引存在即走图会在此变红"
        );
    }
}
