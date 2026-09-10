//! 检索融合器与精排钩子(`rerank.rs`)。
//!
//! 从 `search_builder.rs` 拆出的检索后处理类型:`Fusion`(双通道融合)、
//! [`QueryCtx`] 与 [`Reranker`](精排钩子)。公开签名在 L1 冻结(设计 03 §2.2)。

use crate::memory::record::Hit;

/// Reciprocal Rank Fusion 的缺省平滑常数。
const RRF_K: u32 = 60;

/// 融合器(双通道检索;L1 未落地,单独设置即返回 `Unsupported`)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fusion {
    /// Reciprocal Rank Fusion,默认 `k=60`。
    Rrf {
        /// 平滑常数。
        k: u32,
    },
    /// 加权融合,`alpha ∈ [0,1]`。
    Weighted {
        /// 向量通道权重。
        alpha: f32,
    },
}

impl Default for Fusion {
    fn default() -> Self {
        Self::Rrf { k: RRF_K }
    }
}

/// 精排钩子的查询上下文。
#[derive(Debug, Clone, Copy)]
pub struct QueryCtx<'a> {
    /// 查询文本。
    pub text: Option<&'a str>,
    /// 查询向量。
    pub vector: Option<&'a [f32]>,
}

/// 精排钩子。
pub trait Reranker: Send + Sync {
    /// 对命中列表重排;实现不得改变命中集合语义。
    fn rerank(&self, ctx: &QueryCtx<'_>, hits: Vec<Hit>) -> Vec<Hit>;
}
