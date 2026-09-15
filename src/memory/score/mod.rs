//! 记忆感知排序与沉淀(`score/`)。
//!
//! L1 提供最小内存实现:综合打分(相似度/新鲜度/重要度/访问/可信度)、
//! MMR 多样性、以及基于连通分量的记忆沉淀。默认全部关闭,行为退化为纯相似度。

mod cosine;
mod diversity;
mod rerank;
mod types;

pub(crate) use cosine::{cosine_from_norms, cosine_sim};
pub(crate) use diversity::{cluster_by_similarity, mmr_select};
pub(crate) use rerank::{CompositeRerank, rerank_composite};
pub use types::{ConsolidateReport, ConsolidationPolicy, ScoreBreakdown, Summarizer};

#[cfg(test)]
use cosine::COSINE_PAIRS;

#[cfg(test)]
mod tests;
