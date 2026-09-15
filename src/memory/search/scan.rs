//! 暴力扫描:分块(可选并行)打分、`TopK` 归并,以及两阶段 f32 精排重排。

use crate::core::error::{MnemeError, Result};
use crate::core::heap::TopK;
use crate::core::metric::{Metric, Score};
use crate::core::types::{RowId, SlotId};
use crate::memory::table::ReaderView;

use super::entry::{Scored, SearchParams};

// 单测操作计数:统计扫描阶段的打分次数(线程局部,避免测试间干扰)。
#[cfg(test)]
thread_local! {
    pub(super) static SCORE_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn bump_score_calls() {
    SCORE_CALLS.with(|calls| calls.set(calls.get() + 1));
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

/// 决定分块大小与线程数并执行一次暴力扫描。
pub(super) fn run_scan(
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
    let threads = if cfg!(feature = "wasm") {
        1
    } else if params.parallelism == 0 {
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    } else {
        params.parallelism
    };
    (chunk, threads)
}

/// 两阶段精排排序键:先按 [`Metric::score_order`] 的全序(`NaN` 恒排最后),
/// 再按 `RowId` 升序去平。与 [`TopK`] 选用同一全序,保证「入选集合」与
/// 「最终排序」一致。
pub(super) fn compare_scored(metric: Metric, left: &Scored, right: &Scored) -> std::cmp::Ordering {
    metric
        .score_order(left.score, right.score)
        .then_with(|| left.rowid.cmp(&right.rowid))
}

/// 重新取分:粗排 `TopK` 只提供候选,分数一律用 f32 原向量在候选集内重算并
/// **按 f32 分重排**(两阶段第二阶段;I12 / FC-QUANT-INV-015)。
///
/// 排序口径与 [`TopK`] 一致:良者在前(`Metric::better` 方向),同分按 `RowId` 升序。
pub(super) fn rescore(
    params: &SearchParams<'_>,
    top: TopK<(RowId, SlotId)>,
    query_norm: f32,
) -> Vec<Scored> {
    let mut scored: Vec<Scored> = top
        .into_sorted_vec()
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
        .collect();
    scored.sort_by(|left, right| compare_scored(params.metric, left, right));
    scored
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
        let handles: Vec<_> = params
            .candidates
            .chunks(chunk)
            .map(|part| scope.spawn(move || scan_chunk(params, part)))
            .collect();
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

/// 对单个候选分片打分为局部 `TopK`。
fn scan_chunk(params: &ScanParams<'_>, part: &[u32]) -> TopK<(RowId, SlotId)> {
    let mut local = TopK::new(params.k, params.metric);
    for &idx in part {
        let (rowid, slot, score) = score_slot(params, idx);
        local.push(score, (rowid, slot));
    }
    local
}
