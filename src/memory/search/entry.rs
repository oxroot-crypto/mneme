//! 检索入口与输入类型:过滤先行收集候选,再按前缀规模分派暴力或 ANN 路径。

use crate::core::error::Result;
use crate::core::metric::{Metric, Score};
use crate::core::simd;
use crate::core::types::{NsId, RowId, SlotId};
use crate::memory::pred::{self, EvalCtx, Expr};
use crate::memory::table::ReaderView;

use super::ann::search_indexed;
use super::scan::{rescore, run_scan};

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
    /// 两阶段粗排过采样倍率(粗排候选 = `top_k × 本值`;仅存在量化段时生效)。
    pub(crate) rescore_oversample: usize,
    /// 预计算的候选槽位(过滤先行,与 BM25 通道共享);
    /// `None` = 本函数内自行收集。
    pub(crate) candidates: Option<&'a [u32]>,
    /// 重要性偏置(只改 HNSW 遍历顺序;`Scoring::bias_routing`,FC-SCORE-POST-007)。
    pub(crate) bias: Option<&'a dyn crate::memory::index::NodeBias>,
}

/// 过滤先行的候选收集输入(与 BM25 通道共享同一份候选)。
pub(crate) struct CandidateQuery<'a> {
    /// 不可变读视图。
    pub(crate) view: &'a ReaderView,
    /// 目标命名空间。
    pub(crate) ns_id: NsId,
    /// 预过滤表达式。
    pub(crate) filter: Option<&'a Expr>,
    /// 当前时刻(Unix 毫秒)。
    pub(crate) now_ms: i64,
}

/// 在给定快照上做过滤 + 检索,返回按 `Metric::better` 排序的 top-k。
pub(crate) fn search(params: &SearchParams<'_>) -> Result<Vec<Scored>> {
    let query_norm = if params.metric.needs_norm() {
        norm_sq(params.query)
    } else {
        0.0
    };

    let candidates_owned;
    let candidates: &[u32] = match params.candidates {
        Some(list) => list,
        None => {
            candidates_owned = collect_candidates(&CandidateQuery {
                view: params.view,
                ns_id: params.ns_id,
                filter: params.filter,
                now_ms: params.now_ms,
            });
            &candidates_owned
        }
    };
    let k = params.top_k.min(candidates.len());
    if k == 0 {
        return Ok(Vec::new());
    }

    // L3/L5:各已落盘段图分别 ANN,未覆盖槽位(未落盘尾部/无 hidx 段)暴力,
    // 归并后统一重取分(多段形态下语义与全量暴力统计等价,设计 05 §9)。
    let indexed: usize = params
        .view
        .indexes
        .iter()
        .map(|segment| segment.covered.count_ones())
        .sum();
    if indexed > params.brute_force_max_rows {
        // 段覆盖并集 == 全部槽位(L5 分段语义:槽位只属于一个段)→ `tail` 恒空,
        // 免去每查询 $O(N)$ 的未覆盖收集与全尺寸位图并集。
        let fully_covered = indexed == params.view.slots.len();
        return search_indexed(params, candidates, query_norm, fully_covered);
    }

    let top = run_scan(params, candidates, query_norm, k)?;
    Ok(rescore(params, top, query_norm))
}

/// 过滤先行:只求值元数据,得到候选槽位(不读向量)。
pub(crate) fn collect_candidates(query: &CandidateQuery<'_>) -> Vec<u32> {
    let mut candidates: Vec<u32> = Vec::new();
    let uses_access = query.filter.is_some_and(pred::Expr::uses_access);
    for (idx, slot) in query.view.slots.iter().enumerate() {
        if query.view.dead.get(idx) || slot.ns_id != query.ns_id || !slot.is_live(query.now_ms) {
            continue;
        }
        if let Some(expr) = query.filter {
            let ctx = EvalCtx {
                slot,
                access: uses_access
                    .then(|| query.view.access.get(&slot.rowid).copied())
                    .flatten(),
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
