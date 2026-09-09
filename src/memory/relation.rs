//! 记忆关系边与联想扩展参数(`relation.rs`)。
//!
//! 关系边以 `(from, to, kind)` 为唯一键,重复 `relate` 为 upsert;任一端被删除
//! 则边视为悬挂、不可见(不变量 I25)。语义见设计 09 §2。

use std::collections::HashMap;

use crate::core::meta::Meta;
use crate::core::options::RelationKind;
use crate::core::types::RowId;

/// 联想扩展的缺省跳数。
const DEFAULT_HOPS: u8 = 1;
/// 联想扩展的最大跳数(防止爆炸式遍历)。
pub(crate) const MAX_EXPAND_HOPS: u8 = 3;
/// 联想扩展的缺省衰减系数。
const DEFAULT_EXPAND_DECAY: f32 = 0.5;
/// 联想扩展的缺省节点上限。
const DEFAULT_MAX_NODES: usize = 4096;

/// 一条有向带权关系边。
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    /// 起点。
    pub from: RowId,
    /// 终点。
    pub to: RowId,
    /// 关系类型。
    pub kind: RelationKind,
    /// 边权,`[0,1]`,影响联想扩展传播强度。
    pub weight: f32,
    /// 边元数据(开放 JSON)。
    pub metadata: Meta,
}

/// 关系联想扩展参数(实现见 L10;L1 提供最小内存实现)。
#[derive(Debug, Clone, PartialEq)]
pub struct RelationExpand {
    /// 扩展跳数,默认 1,最大 3。
    pub hops: u8,
    /// 参与扩展的关系类型;空 = 全部类型。
    pub kinds: Vec<RelationKind>,
    /// 每跳衰减系数,默认 0.5。
    pub decay: f32,
    /// 扩展节点数上限,默认 4096。
    pub max_nodes: usize,
}

impl Default for RelationExpand {
    fn default() -> Self {
        Self {
            hops: DEFAULT_HOPS,
            kinds: Vec::new(),
            decay: DEFAULT_EXPAND_DECAY,
            max_nodes: DEFAULT_MAX_NODES,
        }
    }
}

/// 以 `(from, to, kind)` 为键 upsert 一条边到邻接表。
pub(crate) fn upsert_edge(map: &mut HashMap<RowId, Vec<Edge>>, edge: Edge) {
    let bucket = map.entry(edge.from).or_default();
    if let Some(existing) = bucket
        .iter_mut()
        .find(|candidate| candidate.to == edge.to && candidate.kind == edge.kind)
    {
        *existing = edge;
    } else {
        bucket.push(edge);
    }
}

/// 从邻接表移除 `(from, to, kind)`,返回是否命中。
pub(crate) fn remove_edge(
    map: &mut HashMap<RowId, Vec<Edge>>,
    from: RowId,
    to: RowId,
    kind: RelationKind,
) -> bool {
    let Some(bucket) = map.get_mut(&from) else {
        return false;
    };
    let before = bucket.len();
    bucket.retain(|edge| !(edge.to == to && edge.kind == kind));
    let removed = bucket.len() != before;
    if bucket.is_empty() {
        map.remove(&from);
    }
    removed
}
