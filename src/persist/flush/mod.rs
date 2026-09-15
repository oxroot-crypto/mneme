//! 增量段物化:把未落盘槽位写成新段(设计 04 §3.2、07 §4)。
//!
//! `flush` 只物化 `slot_segment == None` 的槽位与自上次 flush 的访问/关系 delta;
//! 旧段保持活跃、不改写、不入 `trash/`。`watermark` 推进后 WAL 方可 Checkpoint。
//! 每条写入已先追加 WAL,故 flush 只是把已确认状态转成段文件。
//!
//! # 子模块
//!
//! * `types` —— 段物化个共享输入/产物同中间态。
//! * `encode` —— 段构建编排同 `vsec`/`msec` 编码。
//! * `quant` —— 量化副本规划、抽样召回评估同两阶段精排。
//! * `delta` —— 访问/关系增量条目收集。
//! * `index` —— 段内 zone map/bloom/倒排同 HNSW 图构建。

mod delta;
mod encode;
mod index;
mod quant;
mod types;

// 拆分唔改外部可达性:旧路径 `crate::persist::flush::{...}` 照旧。
pub(crate) use delta::build_delta;
pub(crate) use encode::build_segment;
pub(crate) use types::{EncodedSegment, SegmentBuildInput};

// 测试经 `use super::*` 取箇滴名字(拆分前由本模块顶层 import 提供)。
#[cfg(test)]
use crate::memory::config::Config;
#[cfg(test)]
use crate::memory::table::WriterState;
#[cfg(test)]
use crate::persist::msec;
#[cfg(test)]
use index::build_indexes;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
mod tests;
