//! 图重建(`rebuild.rs`,设计 05 §7)。
//!
//! 墓碑删除不做单点重连;物理清理交给 compaction 的整体重建,重建即对新节点集
//! 重新执行 §4 构建(墓碑/超期版本不再进入节点集)。

use crate::core::metric::Metric;
use crate::core::options::HnswParams;
use crate::memory::index::IndexNode;

use super::hnsw::HnswIndex;

/// 由新节点集整体重建 HNSW 图。
pub(crate) fn rebuild(nodes: &[IndexNode], params: HnswParams, metric: Metric) -> HnswIndex {
    HnswIndex::build(nodes, params, metric)
}
