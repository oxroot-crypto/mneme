//! 大 flush 切块与块级并行度:块行数、并行度同内存预算约束。

/// 大 flush 切块行数:取 `Tuning.flush_chunk_rows`(默认 65_536;`< 1` 收敛为 1)。
///
/// 小于该规模保持「一次 flush = 一段」语义;大规模建库按此切块,块数 =
/// ⌈行数/本值⌉(每块一个段),并行度只限制**同时**构建的块数。
/// 库本体绝不读取环境变量(设计 16 §6):重载调参请经 `Builder::tuning`。
pub(in crate::persist::store) fn flush_chunk_rows(tuning: &crate::core::options::Tuning) -> usize {
    tuning.flush_chunk_rows.max(1)
}

/// 并行构建的内存预算(字节):单个构建块的编码缓冲约占
/// `块行数 × (维度 × 4 + 256) × 2`(vsec f32 区 + msec/hidx/量化副本与临时
/// 缓冲)。超过预算时降低同时构建的块数,避免大维度多核并行触发换页(swap)。
const FLUSH_BUILD_MEMORY_BUDGET: u64 = 6 * 1024 * 1024 * 1024;

/// 块级并行构建度:取 `Tuning.flush_threads`(默认 1 = 块级串行,由块内批并行
/// 承担加速;实测 4 核机上嵌套并行因带宽争抢反而更慢)。
///
/// 库本体绝不读取环境变量(设计 16 §6):重载调参请经 `Builder::tuning`。
pub(in crate::persist::store) fn flush_parallelism(tuning: &crate::core::options::Tuning) -> usize {
    tuning.flush_threads.max(1)
}

/// 并行构建的块数:受并行度、块数与内存预算三者约束。
///
/// `dimension`/`chunk_rows` 用于估算单块编码缓冲;预算不足时宁可少并行,
/// 也不让多个大块同时驻留把机器拖进 swap(内存带宽已使并行收益递减)。
pub(super) fn flush_build_threads(
    dimension: usize,
    chunk_rows: usize,
    parallelism: usize,
    chunk_count: usize,
) -> usize {
    let per_block = (chunk_rows as u64)
        .saturating_mul((dimension as u64).saturating_mul(4).saturating_add(256))
        .saturating_mul(2)
        .max(1);
    let by_memory = (FLUSH_BUILD_MEMORY_BUDGET / per_block).max(1) as usize;
    parallelism.min(chunk_count).min(by_memory).max(1)
}

/// 把本次物化槽位按 [`flush_chunk_rows`] 切分为构建块。
///
/// 每块至多配置的块行数;块数由行数决定,并行度只限制**同时**构建的块数
/// (见 `encode_flush_segments`),不放大单块规模。空槽位(纯 delta 段)同样返回
/// 一个空块:块与段一一对应,供 MANIFEST/安装使用。
pub(super) fn split_slot_chunks<'a>(
    slots: &'a [usize],
    tuning: &crate::core::options::Tuning,
) -> Vec<&'a [usize]> {
    split_slot_chunks_with(slots, flush_chunk_rows(tuning))
}

/// 按给定块行数切分(便于单测覆盖多块边界;生产经 [`split_slot_chunks`])。
pub(super) fn split_slot_chunks_with(slots: &[usize], chunk_rows: usize) -> Vec<&[usize]> {
    if slots.is_empty() {
        return vec![&[]];
    }
    // reason: 调用方保证 `chunk_rows > 0`(`flush_chunk_rows` 已把 `< 1` 收敛为 1);
    // 此处再夹一次是防御性兜底,避免 `chunks(0)` panic。
    slots.chunks(chunk_rows.max(1)).collect()
}
