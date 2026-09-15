//! 增量段 flush 与备份(`store/snapshot/`)。
//!
//! `flush` 只把未落盘槽位与跨段 delta 物化为一个新段、提交新 MANIFEST 并
//! Checkpoint WAL(重置);已提交旧段保持活跃、write-once、不入 `trash/`。
//! `backup_to` 复制全部文件到目标目录,最后写 `current` 保证备份原子可用。
//!
//! # 子模块
//!
//! * `chunk` —— 大 flush 切块、块级并行度同内存预算。
//! * `commit` —— 增量段提交:切块编码、新段写出同 MANIFEST 提交。
//! * `manifest` —— 新段 MANIFEST 版本构造同快照发布。
//! * `backup` —— `backup_to` 备份复制(硬链接优先)。

mod backup;
mod chunk;
mod commit;
mod manifest;

// 拆分唔改外部可达性:旧路径 `crate::persist::store::snapshot::{flush_chunk_rows, flush_parallelism}` 照旧。
pub(super) use chunk::{flush_chunk_rows, flush_parallelism};

// 测试经 `use super::*` 取箇滴名字(拆分前由本模块顶层 import 提供)。
#[cfg(test)]
use crate::persist::storage;
#[cfg(test)]
use crate::persist::storage::SEGMENTS_DIR;
#[cfg(test)]
use backup::{CopyCounts, CopySink};
#[cfg(test)]
use chunk::{flush_build_threads, split_slot_chunks, split_slot_chunks_with};

#[cfg(test)]
mod tests;
