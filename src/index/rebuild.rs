//! 图重建(`rebuild.rs`,设计 05 §7)。
//!
//! 墓碑删除不做单点重连;物理清理交给 compaction 的整体重建,重建即对新节点集
//! 重新执行 §4 构建。**L3 的构建输入是段内全部物理版本(含墓碑与历史版本)**:
//! 死节点只可穿越、不可入选(设计 05 §7),节点集剔除属 L5 compaction 的目标。

use crate::core::metric::Metric;
use crate::core::options::HnswParams;
use crate::memory::index::IndexNode;

use super::hnsw::HnswIndex;

/// 由新节点集整体重建 HNSW 图。
pub(crate) fn rebuild(nodes: &[IndexNode], params: HnswParams, metric: Metric) -> HnswIndex {
    HnswIndex::build(nodes, params, metric)
}
