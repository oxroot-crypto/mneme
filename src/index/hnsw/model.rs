//! HNSW 索引状态与跨子模块共享的数据类型。

use crate::core::metric::Metric;
use crate::core::types::SlotId;
use crate::index::graph::GraphStore;
use crate::memory::index::{IndexNode, QuantCopy};

/// 单节点的建图计划(批内并行计算、批间串行应用;`FC-INDEX-POST-012`)。
pub(super) struct LinkPlan {
    /// 各层选中的邻居 `(层号, 邻居节点)`;按层号降序(与构建下降顺序一致)。
    pub(super) layers: Vec<(usize, Vec<u32>)>,
}

/// `Hybrid` 建图档的段内临时 i8 码流(不落盘,构建结束释放)。
///
/// 节点顺序与 `nodes` 一致;参数/编码复用 `FC-QUANT-POST-001` 的段级逐维口径,
/// 误差上界同为 `Δ/2`(`FC-INDEX-POST-010`)。
pub(super) struct BuildCodes {
    /// 段级逐维量化参数。
    pub(super) params: crate::quant::scalar_i8::I8Params,
    /// 连续码流(`count × dimension` 字节)。
    pub(super) codes: Vec<u8>,
}

impl BuildCodes {
    /// 第 `node` 个节点的码流切片(节点序号越界或维数为 0 返回 `None`)。
    pub(super) fn row(&self, node: usize) -> Option<&[u8]> {
        let dimension = self.params.dimension();
        if dimension == 0 {
            return None;
        }
        let start = node.checked_mul(dimension)?;
        self.codes.get(start..start.checked_add(dimension)?)
    }
}

/// 建图节点与图的只读访问(供 `filtered` 与上层查询)。
pub(crate) struct HnswIndex {
    pub(super) nodes: Vec<IndexNode>,
    pub(super) slot_of: Vec<SlotId>,
    /// 图存储:构建期为堆图,hidx 载入期为惰性映射图(FC-PERSIST-INV-021)。
    pub(super) graph: GraphStore,
    pub(super) m: usize,
    pub(super) m0: usize,
    pub(super) ml: f32,
    pub(super) ef_construction: usize,
    pub(super) metric: Metric,
    /// 段的量化副本(节点顺序与 `nodes` 对齐);`None` = 纯 f32。
    pub(super) quant: Option<QuantCopy>,
    /// i8 副本的段级参数解析结果(载入/构建时一次,查询期复用,免每查询重解析)。
    pub(super) i8_params: Option<crate::quant::scalar_i8::I8Params>,
    /// `Hybrid` 建图档的临时码流;`F32` 档与载入路径恒为 `None`。
    pub(super) build: Option<BuildCodes>,
    /// 启发式选邻的「新方向」比较上限(构建期由 `Tuning` 派生;载入路径为默认值)。
    pub(super) compare_cap: usize,
}
