//! ANN 路径:逐段图搜索 + 未覆盖尾部暴力补扫,归并后统一重排。

use std::sync::Arc;

use crate::core::bitset::BitSet;
use crate::core::error::{MnemeError, Result};
use crate::core::heap::TopK;
use crate::core::options::VectorFormat;
use crate::core::types::{RowId, SlotId};
use crate::memory::index::IndexSearch;

use super::entry::{Scored, SearchParams};
use super::scan::{rescore, run_scan};

// 单测操作计数:统计 ANN 粗排候选数与段 alive 位图缓存命中(线程局部,避免测试间干扰)。
#[cfg(test)]
thread_local! {
    pub(super) static COARSE_CANDIDATES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static SEGMENT_ALIVE_CACHE_HITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(super) static SEGMENT_ALIVE_CACHE_STORES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_coarse_candidates(k: usize) {
    COARSE_CANDIDATES.with(|calls| calls.set(k));
}

/// ANN 路径:存在量化段时粗排放宽到 `top_k × rescore_oversample`(默认 4 倍,即
/// 设计中的「4k」),f32 精排按 f32 分重排再截回 `top_k`(I12 / FC-QUANT-INV-015);
/// 无量化段时口径与 L3 完全一致。
pub(super) fn search_indexed(
    params: &SearchParams<'_>,
    candidates: &[u32],
    query_norm: f32,
    fully_covered: bool,
) -> Result<Vec<Scored>> {
    let quantized = params
        .view
        .indexes
        .iter()
        .any(|segment| segment.quant != VectorFormat::F32);
    let coarse_k = if quantized {
        crate::quant::rescore::coarse_candidates(params.top_k, params.rescore_oversample)
            .min(candidates.len())
    } else {
        params.top_k.min(candidates.len())
    };
    let budget = AnnBudget {
        query_norm,
        k: coarse_k,
    };
    let mut scored = ann_search(params, candidates, budget, fully_covered)?;
    scored.truncate(params.top_k);
    Ok(scored)
}

/// ANN 路径的预算参数:`search` 已算好查询范数与有效 `k`,打包传入避免参数堆叠。
#[derive(Clone, Copy)]
struct AnnBudget {
    query_norm: f32,
    k: usize,
}

/// ANN 路径:逐段图搜索 + 未覆盖候选暴力,归并后再取分。
///
/// 段之间相互独立(各自的图与位图只读):段数 ≥ 2 时按可用核数并行搜索,
/// 每段结果按段号回填后**按段序归并**,与串行执行逐位一致。
fn ann_search(
    params: &SearchParams<'_>,
    candidates: &[u32],
    budget: AnnBudget,
    fully_covered: bool,
) -> Result<Vec<Scored>> {
    let AnnBudget { query_norm, k } = budget;
    #[cfg(test)]
    record_coarse_candidates(k);
    let segments = params.view.indexes.as_slice();
    let partials = ann_search_segments(params, segments, candidates, budget)?;
    let mut top = ann_merge_top(params, partials, k);
    let tail = ann_uncovered_tail(segments, candidates, fully_covered);
    if !tail.is_empty() {
        top.merge(run_scan(params, &tail, query_norm, k)?);
    }
    Ok(rescore(params, top, query_norm))
}

/// 单段图搜索:只处理与该段覆盖相交的候选;`alive` 亦限制在本段覆盖内,
/// 避免选择性口径被其他段的位图稀释(过滤三档按段独立分派)。
fn ann_segment_search(
    params: &SearchParams<'_>,
    segment: &crate::memory::index::SegmentIndex,
    candidates: &[u32],
    budget: AnnBudget,
) -> TopK<(RowId, SlotId)> {
    let (alive, filter) = segment_bitmaps(params, segment, candidates);
    segment.index.search(&IndexSearch {
        query: params.query,
        query_norm: budget.query_norm,
        ef: params.ef,
        k: budget.k,
        alive: alive.as_ref(),
        filter: filter.as_ref(),
        post_threshold: params.filter_post_threshold,
        brute_threshold: params.filter_brute_threshold,
        use_quant: segment.quant != VectorFormat::F32,
        bias: params.bias,
    })
}

/// 段搜索 worker 数:取可用核数(wasm 下固定 1)与段数的较小值。
fn ann_search_workers(segment_count: usize) -> usize {
    if cfg!(feature = "wasm") {
        1
    } else {
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    }
    .min(segment_count)
}

/// 逐段执行图搜索:单核或仅一段时串行,否则按段并行(结果均按段序返回)。
fn ann_search_segments(
    params: &SearchParams<'_>,
    segments: &[crate::memory::index::SegmentIndex],
    candidates: &[u32],
    budget: AnnBudget,
) -> Result<Vec<TopK<(RowId, SlotId)>>> {
    if ann_search_workers(segments.len()) <= 1 {
        return Ok(segments
            .iter()
            .map(|segment| ann_segment_search(params, segment, candidates, budget))
            .collect());
    }
    ann_parallel_segments(params, segments, candidates, budget)
}

/// 动态游标分派:每个 worker 处理若干段,结果按段号回填(顺序与串行一致)。
fn ann_parallel_segments(
    params: &SearchParams<'_>,
    segments: &[crate::memory::index::SegmentIndex],
    candidates: &[u32],
    budget: AnnBudget,
) -> Result<Vec<TopK<(RowId, SlotId)>>> {
    let workers = ann_search_workers(segments.len());
    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let mut slots: Vec<Option<TopK<(RowId, SlotId)>>> = (0..segments.len()).map(|_| None).collect();
    std::thread::scope(|scope| -> Result<()> {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| -> Vec<(usize, TopK<(RowId, SlotId)>)> {
                    let mut produced = Vec::new();
                    loop {
                        let index = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(segment) = segments.get(index) else {
                            break;
                        };
                        produced.push((
                            index,
                            ann_segment_search(params, segment, candidates, budget),
                        ));
                    }
                    produced
                })
            })
            .collect();
        for handle in handles {
            let produced = handle.join().map_err(|_| MnemeError::Inconsistent {
                reason: "ANN 段搜索线程 panic",
            })?;
            for (index, partial) in produced {
                slots[index] = Some(partial);
            }
        }
        Ok(())
    })?;
    slots
        .into_iter()
        .map(|slot| {
            slot.ok_or(MnemeError::Inconsistent {
                reason: "ANN 段搜索结果缺失",
            })
        })
        .collect()
}

/// 按段序归并各段局部 TopK。
fn ann_merge_top(
    params: &SearchParams<'_>,
    partials: Vec<TopK<(RowId, SlotId)>>,
    k: usize,
) -> TopK<(RowId, SlotId)> {
    let mut top = TopK::new(k, params.metric);
    for partial in partials {
        top.merge(partial);
    }
    top
}

/// 收集未被任何已搜索段覆盖的候选(全覆盖时为空)。
fn ann_uncovered_tail(
    segments: &[crate::memory::index::SegmentIndex],
    candidates: &[u32],
    fully_covered: bool,
) -> Vec<u32> {
    if fully_covered {
        return Vec::new();
    }
    let mut covered = BitSet::default();
    for segment in segments {
        covered.union_with(&segment.covered);
    }
    candidates
        .iter()
        .copied()
        .filter(|&idx| !covered.get(idx as usize))
        .collect()
}

/// 构造单个段索引的 `alive` 位图与候选过滤位图(均按全局槽位)。
///
/// 无用户过滤且段内该命名空间无 TTL 行时,`alive` 只依赖不可变视图,按
/// `(段号, NsId)` 复用 [`ReaderView`](crate::memory::table::ReaderView) 上的视图级缓存(`FC-QUERY-POST-008`);
/// 含 TTL 行或有过滤时不写缓存,行为与逐段重建完全一致。
fn segment_bitmaps(
    params: &SearchParams<'_>,
    segment: &crate::memory::index::SegmentIndex,
    candidates: &[u32],
) -> (Arc<BitSet>, Option<BitSet>) {
    if params.filter.is_none()
        && let Some(cached) = params
            .view
            .cached_segment_alive(segment.segment_id, params.ns_id)
    {
        #[cfg(test)]
        SEGMENT_ALIVE_CACHE_HITS.with(|hits| hits.set(hits.get() + 1));
        return (cached, None);
    }
    let (alive, has_ttl) = build_segment_alive(params, segment);
    let alive = Arc::new(alive);
    if params.filter.is_none() && !has_ttl {
        #[cfg(test)]
        SEGMENT_ALIVE_CACHE_STORES.with(|stores| stores.set(stores.get() + 1));
        params
            .view
            .store_segment_alive(segment.segment_id, params.ns_id, Arc::clone(&alive));
    }
    (alive, segment_candidate_filter(params, segment, candidates))
}

/// 逐槽构建段内活行位图(按全局槽位),并报告段内该命名空间是否存在 TTL 行。
fn build_segment_alive(
    params: &SearchParams<'_>,
    segment: &crate::memory::index::SegmentIndex,
) -> (BitSet, bool) {
    let mut alive = BitSet::with_capacity_bits(params.view.slots.len());
    let mut has_ttl = false;
    for slot in &segment.slots {
        let idx = slot.get() as usize;
        let Some(slot_data) = params.view.slots.get(idx) else {
            continue;
        };
        if slot_data.ns_id == params.ns_id && slot_data.expires_at.is_some() {
            has_ttl = true;
        }
        if !params.view.dead.get(idx)
            && slot_data.ns_id == params.ns_id
            && slot_data.is_live(params.now_ms)
        {
            alive.set(idx);
        }
    }
    (alive, has_ttl)
}

/// 仅当存在用户过滤(`filter`)时构造索引段过滤位图:exec 无过滤时传全 1
/// 候选且 `filter = None`,不得把"全 1 候选"误当过滤(否则会触发契约
/// FC-INDEX-POST-001 的档③「候选暴力」路径)。
fn segment_candidate_filter(
    params: &SearchParams<'_>,
    segment: &crate::memory::index::SegmentIndex,
    candidates: &[u32],
) -> Option<BitSet> {
    params.filter.is_some().then(|| {
        let mut bits = BitSet::default();
        for &idx in candidates {
            let idx = idx as usize;
            if segment.covered.get(idx) {
                bits.set(idx);
            }
        }
        bits
    })
}
