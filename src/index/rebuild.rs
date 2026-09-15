//! 图重建(`rebuild.rs`,设计 05 §7)。
//!
//! 墓碑删除不做单点重连;物理清理交给 compaction 的整体重建,重建即对新节点集
//! 重新执行 §4 构建。**L3 的构建输入是段内全部物理版本(含墓碑与历史版本)**:
//! 死节点只可穿越、不可入选(设计 05 §7),节点集剔除属 L5 compaction 的目标。

use crate::core::error::Result;
use crate::memory::index::IndexBuildRequest;

use super::hnsw::{BuildWithSlotsInput, HnswIndex};

/// 由新节点集整体重建 HNSW 图,并显式给出节点到全局槽位的映射。
///
/// 增量段只覆盖部分全局槽位,节点 id 与全局槽位不再恒等,必须显式映射
/// (设计 07 §4;查询期 `alive`/过滤位图按全局槽位索引)。
/// 构建档位/并行度语义见 [`IndexBuildRequest`]。
///
/// # Errors
/// 建图期段内量化或索引装配失败时返回结构化错误,绝不静默降级档位。
pub(crate) fn rebuild_with_slots(request: IndexBuildRequest<'_>) -> Result<HnswIndex> {
    HnswIndex::build_with_slots(BuildWithSlotsInput {
        nodes: request.nodes,
        slot_of: request.slot_of,
        params: request.params,
        metric: request.metric,
        quant: request.quant,
        precision: request.build_precision,
        build: request.build,
    })
}
