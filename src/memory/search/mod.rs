//! 暴力扫描 / ANN 检索(`search/`)。
//!
//! **过滤先行**:先用元数据求值得到候选位图(不读向量),再分块并行打分、
//! `TopK` 归并。这是设计 03 §4 的 L1 落地。L3 起:若读视图带有覆盖槽位前缀的
//! 向量索引且前缀规模超过 `brute_force_max_rows`,前缀走 HNSW(过滤三档),
//! 未建树的尾部仍暴力扫描,二者 `TopK` 归并——语义与纯暴力统计等价(设计 05 §9)。

mod ann;
mod entry;
mod scan;

pub(crate) use entry::{Scored, SearchParams, norm_sq, search};

#[cfg(test)]
pub(crate) use entry::{CandidateQuery, collect_candidates};

#[cfg(test)]
mod tests;
