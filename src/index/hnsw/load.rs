//! hidx 载入、量化查询形式与 `VectorIndex` trait 实现。

use crate::core::error::Result;
use crate::core::heap::TopK;
use crate::core::metric::Metric;
use crate::core::options::{HnswBuildParams, VectorFormat};
use crate::core::types::{RowId, SlotId};
use crate::index::graph::GraphStore;
use crate::index::{filtered, hidx};
use crate::memory::index::{IndexNode, IndexSearch, QuantCopy, QuantQuery, VectorIndex};

use super::HnswIndex;
use super::quant::{cached_i8_params, validate_quant};

/// [`HnswIndex::load`] 的输入参数。
#[derive(Debug)]
pub(crate) struct HnswLoadInput<'a> {
    /// hidx 文件视图(只读头部与 `node_table`,邻接按需解码)。
    pub(crate) span: &'a crate::memory::lazy::ByteSpan,
    /// 段内节点顺序的输入(与 hidx 节点 id 对齐)。
    pub(crate) nodes: &'a [IndexNode],
    /// 段内节点 id → 全局槽位(恢复重排映射)。
    pub(crate) slot_of: &'a [SlotId],
    /// 库距离度量(建库即锁定,不存于 hidx)。
    pub(crate) metric: Metric,
    /// 从 vsec qvec 区还原的段级副本(`None` = 纯 f32)。
    pub(crate) quant: Option<QuantCopy>,
}

impl HnswIndex {
    /// 由 hidx 句柄载入图(节点顺序与 `nodes`/`slot_of` 对齐)。
    ///
    /// 只解析头部与 `node_table`;邻接字节随句柄按需解码(FC-PERSIST-INV-021)。
    /// 图参数以 hidx 头部为准;`metric` 取自库配置(建库即锁定);`quant`
    /// 为从 vsec qvec 区还原的段级副本。
    ///
    /// # Errors
    /// hidx 解析失败或节点数/量化副本不一致时返回结构化错误。
    pub(crate) fn load(input: HnswLoadInput<'_>) -> Result<Self> {
        let mapped = hidx::open(input.span)?;
        if mapped.node_count() != input.nodes.len() || input.slot_of.len() != input.nodes.len() {
            return Err(crate::core::error::MnemeError::Corrupted {
                segment: None,
                reason: "hidx: 节点数与恢复槽位数不一致".to_string(),
            });
        }
        let dimension = input.nodes.first().map_or(0, |node| node.vector.len());
        validate_quant(&input.quant, input.nodes.len(), dimension)?;
        let params = mapped.params();
        let i8_params = cached_i8_params(&input.quant, dimension);
        Ok(Self {
            nodes: input.nodes.to_vec(),
            slot_of: input.slot_of.to_vec(),
            graph: GraphStore::Mapped(mapped),
            m: params.m.max(2) as usize,
            m0: params.m0.max(2) as usize,
            ml: params.ml,
            ef_construction: params.ef_construction.max(1) as usize,
            metric: input.metric,
            quant: input.quant,
            i8_params,
            // 载入路径不构建(档位只影响构建期距离,hidx 不含档位元数据)。
            build: None,
            compare_cap: HnswBuildParams::default().compare_cap,
        })
    }

    /// 为本次查询预计算段级量化形式;无副本或维度不符时返回 `None`(退 f32)。
    pub(crate) fn quantize_query(&self, query: &[f32]) -> Option<QuantQuery> {
        let copy = self.quant.as_ref()?;
        match copy.format {
            VectorFormat::F32 => None,
            VectorFormat::I8Rescored => {
                let params = self.i8_params.as_ref()?;
                crate::quant::scalar_i8::Query::new(query, params)
                    .ok()
                    .map(QuantQuery::I8)
            }
            VectorFormat::F16 => {
                #[cfg(feature = "quant-f16")]
                {
                    Some(QuantQuery::F16)
                }
                #[cfg(not(feature = "quant-f16"))]
                {
                    None
                }
            }
        }
    }

    /// 节点 id 对应的全局槽位。
    pub(crate) fn slot_of(&self, node: u32) -> SlotId {
        self.slot_of[node as usize]
    }

    /// 建库时锁定的距离度量(查询与索引用同一度量,避免调用方重复传入而不一致)。
    pub(crate) fn metric(&self) -> Metric {
        self.metric
    }

    /// 节点 id 对应的稳定 `RowId`。
    pub(crate) fn rowid(&self, node: u32) -> RowId {
        self.nodes[node as usize].rowid
    }

    /// 入口节点 id。
    pub(crate) fn entry_node(&self) -> u32 {
        self.graph.entry()
    }
}

impl VectorIndex for HnswIndex {
    fn node_count(&self) -> usize {
        self.nodes.len()
    }

    fn max_level(&self) -> u8 {
        self.graph.max_level()
    }

    fn entry(&self) -> (SlotId, u8) {
        if self.nodes.is_empty() {
            return (SlotId::new(0), 0);
        }
        (
            self.slot_of[self.graph.entry() as usize],
            self.graph.entry_level(),
        )
    }

    fn serialize(&self) -> Result<Vec<u8>> {
        // 惰性图在序列化时物化为堆图(仅 flush/诊断路径;查询不走此路)。
        let graph = self.graph.to_graph();
        hidx::encode(
            &graph,
            hidx::GraphParams {
                m: self.m as u16,
                m0: self.m0 as u16,
                ef_construction: self.ef_construction as u16,
                ml: self.ml,
            },
        )
    }

    fn search(&self, params: &IndexSearch<'_>) -> TopK<(RowId, SlotId)> {
        filtered::search(self, params)
    }
}
