//! `Store::open` 及身份解析 / 状态载入辅助(`store/open/`)。
//!
//! 打开流程:确保目录布局 → 取独占锁 → 载入或初始化 MANIFEST → 载入段并回放
//! WAL 重建写状态 → 打开 WAL 写入器。
//!
//! # 子模块
//!
//! * `options` —— [`OpenOptions`] 打开参数。
//! * `entry` —— [`Store::open`] 与只读重载入口,目录/锁/WAL 配置准备。
//! * `manifest_init` —— MANIFEST 载入或初始化,维度/度量身份校验。
//! * `state_load` —— 载入段并回放 WAL,重建写状态。
//! * `hidx_load` —— 段 hidx 索引与量化副本载入。

mod entry;
mod hidx_load;
mod manifest_init;
mod options;
mod state_load;

// 拆分唔改外部可达性:旧路径 `crate::persist::store::open::{...}` 照旧。
pub(crate) use entry::reload_read_only;
pub(crate) use options::OpenOptions;

// 测试经 `use super::*` 取箇滴名字(拆分前由本模块顶层 import 提供)。
#[cfg(test)]
use crate::core::error::{MnemeError, Result};
#[cfg(test)]
use crate::core::metric::Metric;
#[cfg(test)]
use crate::core::types::SlotId;
#[cfg(test)]
use crate::memory::index::{IndexFactory, IndexLoadRequest, VectorIndex};
#[cfg(test)]
use crate::memory::table::WriterState;
#[cfg(test)]
use hidx_load::{LoadIndexInput, SlotRemap, load_index};
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
mod tests;
