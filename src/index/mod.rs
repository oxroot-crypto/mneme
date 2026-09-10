//! L3 索引层:自研 HNSW、过滤三档搜索与 hidx 持久化(设计 05)。
//!
//! 本层把 L1 的暴力扫描升级为近似最近邻(HNSW),公开 API 不变;依赖 L0
//! (`bitset`/`TopK`/`Metric`/标识类型)与 L1 的索引抽象
//! [`crate::memory::index`](VectorIndex/IndexFactory),不依赖 L2 字节布局之外的实现。
//!
//! # 模块
//!
//! * [`hnsw`] —— HNSW 构建 / 查询 / 启发式选邻。
//! * [`graph`] —— 分层邻接图存储。
//! * [`filtered`] —— 过滤三档策略(后过滤 / 约束遍历 / 候选暴力)。
//! * [`rebuild`] —— compaction 整体重建入口。
//! * [`hidx`] —— HID1 文件编解码。
//!
//! > 跨来源归并(索引前缀 + 未建树尾)复用 L0 的 [`TopK::merge`](crate::core::heap::TopK::merge),
//! > 不单设 `merge.rs`——L2 全量快照下"多段"退化为单图 + 尾扫描,L5 compaction 再引入
//! > 多图归并时恢复该模块(见设计 05 §9)。

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::metric::Metric;
use crate::core::options::HnswParams;
use crate::core::types::SlotId;
use crate::memory::index::{IndexFactory, IndexNode, VectorIndex};

pub(crate) mod filtered;
pub(crate) mod graph;
pub(crate) mod hidx;
pub(crate) mod hnsw;
pub(crate) mod rebuild;

pub(crate) use hnsw::HnswIndex;

/// HNSW 索引工厂(门面经 [`default_factory`] 注入 [`IndexFactory`])。
pub(crate) struct HnswFactory;

impl IndexFactory for HnswFactory {
    fn build(
        &self,
        nodes: &[IndexNode],
        params: HnswParams,
        metric: Metric,
    ) -> Arc<dyn VectorIndex> {
        Arc::new(rebuild::rebuild(nodes, params, metric))
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
    ) -> Result<Arc<dyn VectorIndex>> {
        Ok(Arc::new(HnswIndex::load(bytes, nodes, slot_of, metric)?))
    }
}

/// 默认索引工厂。
pub(crate) fn default_factory() -> Arc<dyn IndexFactory> {
    Arc::new(HnswFactory)
}
