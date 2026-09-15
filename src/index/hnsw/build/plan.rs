//! HNSW 建图批计划(`hnsw/build/plan.rs`)。
//!
//! 批行数与线程数只依赖节点数与建图配置,与线程调度无关(同输入同图)。

use crate::core::options::HnswBuildParams;

/// 由节点数与建图参数推导批行数(小图退化串行;只依赖节点数与配置,与线程数无关)。
pub(in crate::index::hnsw) fn build_batch_rows(count: usize, build: &HnswBuildParams) -> usize {
    if count <= build.serial_rows {
        1
    } else {
        build.batch_rows
    }
}

/// 首批行数:受 `m0` 约束(首批节点都只连得到 0 号节点,超出 `m0` 会被修剪掉
/// 入边导致不可达),保证初始核心图连通。
pub(in crate::index::hnsw) fn build_first_batch_rows(batch_rows: usize, m0: usize) -> usize {
    batch_rows.min(m0.max(1)).max(1)
}

/// 实际建图线程数:`0` 按可用核数,再受批大小与配置硬上限约束。
pub(in crate::index::hnsw) fn build_threads(
    parallelism: usize,
    batch_rows: usize,
    threads_max: usize,
) -> usize {
    let threads = if parallelism == 0 {
        std::thread::available_parallelism().map_or(1, |count| count.get())
    } else {
        parallelism
    };
    threads.clamp(1, threads_max.max(1)).min(batch_rows.max(1))
}
