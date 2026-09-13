//! HNSW 索引工厂:把 `IndexFactory` 接到 [`HnswIndex`](super::hnsw::HnswIndex)。
//!
//! 只做装配(组合根例外,见 `AGENTS.md`),不含索引算法本身;门面经
//! [`default_factory`] 注入 [`IndexFactory`](crate::memory::index::IndexFactory)。

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::metric::Metric;
use crate::core::options::HnswParams;
use crate::core::types::SlotId;
use crate::memory::index::{IndexFactory, IndexNode, QuantCopy, VectorIndex};

use super::hnsw::HnswIndex;
use super::{hidx, rebuild};

/// HNSW 索引工厂。
pub(crate) struct HnswFactory;

impl IndexFactory for HnswFactory {
    fn build(
        &self,
        nodes: &[IndexNode],
        slot_of: &[SlotId],
        params: HnswParams,
        metric: Metric,
        quant: Option<QuantCopy>,
    ) -> Arc<dyn VectorIndex> {
        Arc::new(rebuild::rebuild_with_slots(
            nodes, slot_of, params, metric, quant,
        ))
    }

    fn verify(&self, bytes: &[u8]) -> Result<()> {
        hidx::decode(bytes).map(|_decoded| ())
    }

    fn load(
        &self,
        bytes: &[u8],
        nodes: &[IndexNode],
        slot_of: &[SlotId],
        metric: Metric,
        quant: Option<QuantCopy>,
    ) -> Result<Arc<dyn VectorIndex>> {
        Ok(Arc::new(HnswIndex::load(
            bytes, nodes, slot_of, metric, quant,
        )?))
    }
}

/// 默认索引工厂。
pub(crate) fn default_factory() -> Arc<dyn IndexFactory> {
    Arc::new(HnswFactory)
}
