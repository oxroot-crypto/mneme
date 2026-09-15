//! 段重建与写状态初始化(`recover/state/`)。
//!
//! 把各活跃段的 `vsec`/`msec` 重建成版本链、key 索引与关系边;墓碑以
//! `version_table.doc_offset = TOMBSTONE_DOC_OFFSET` 表示(无记录体),保证
//! "删除永不复活"(I19)。
//!
//! # 子模块
//!
//! * `types` —— 恢复输入/产物同段视图个数据结构。
//! * `parse` —— 逐段解析、版本行收集同损坏段隔离。
//! * `load` —— 写状态初始化同段回放个总编排。
//! * `remap` —— "段内槽位 → 全局槽位"重排映射同槽位段回填。
//! * `index` —— 盘上四区索引个装载同全量重建降级。

mod index;
mod load;
mod parse;
mod remap;
mod types;

// 拆分唔改外部可达性:旧路径 `crate::persist::recover::state::{...}` 照旧。
pub(crate) use load::{empty_state, load_segments};
pub(super) use types::ParsedSegment;
pub(crate) use types::{RecoveredSegments, SegmentBytes, SegmentRemap};

// 测试经 `use super::*` 取箇滴名字(拆分前由本模块顶层 import 提供)。
#[cfg(test)]
use crate::memory::table::WriterState;
#[cfg(test)]
use crate::persist::msec;
#[cfg(test)]
use crate::persist::source::ByteFile;
#[cfg(test)]
use crate::persist::vsec;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use super::segment::apply_relations;
#[cfg(test)]
use index::load_disk_indexes;
#[cfg(test)]
use parse::collect_versions;
#[cfg(test)]
use remap::build_remaps;

#[cfg(test)]
mod tests;
