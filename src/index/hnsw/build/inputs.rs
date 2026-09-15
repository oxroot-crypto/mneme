//! HNSW 构建入口的输入参数(`hnsw/build/inputs.rs`)。

use crate::core::metric::Metric;
use crate::core::options::{BuildPrecision, HnswBuildParams, HnswParams};
use crate::core::types::SlotId;
use crate::memory::index::{IndexNode, QuantCopy};

// `HnswIndex` 只出现在 rustdoc 链接里;放行 unused 警告。
#[allow(unused_imports)]
use crate::index::hnsw::HnswIndex;

/// [`HnswIndex::build_with_slots`] 的输入参数。
#[derive(Debug)]
pub(crate) struct BuildWithSlotsInput<'a> {
    /// 建图节点(节点 id = 下标)。
    pub(crate) nodes: &'a [IndexNode],
    /// 节点 id → 全局槽位映射(与 `nodes` 等长;增量段用)。
    pub(crate) slot_of: &'a [SlotId],
    /// HNSW 图参数。
    pub(crate) params: HnswParams,
    /// 距离度量(建库即锁定)。
    pub(crate) metric: Metric,
    /// 段量化副本(`None` = 纯 f32),只服务查询期粗排打分。
    pub(crate) quant: Option<QuantCopy>,
    /// 建图距离精度档位(设计 05 §4.4、`FC-INDEX-POST-010`)。
    pub(crate) precision: BuildPrecision,
    /// 建图工程参数(并行度/批行数/选邻比较上限,由 `Tuning` 派生)。
    pub(crate) build: HnswBuildParams,
}

/// [`HnswIndex::build_with_options`] 的输入参数。
#[cfg(test)]
#[derive(Debug)]
pub(in crate::index::hnsw) struct BuildWithOptionsInput<'a> {
    /// 建图节点(节点 id = 下标)。
    pub(in crate::index::hnsw) nodes: &'a [IndexNode],
    /// HNSW 图参数。
    pub(in crate::index::hnsw) params: HnswParams,
    /// 距离度量。
    pub(in crate::index::hnsw) metric: Metric,
    /// 建图距离精度档位。
    pub(in crate::index::hnsw) precision: BuildPrecision,
    /// 建图并行度(`0` = 可用核数)。
    pub(in crate::index::hnsw) parallelism: usize,
}
