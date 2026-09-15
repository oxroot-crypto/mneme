//! 自研 HNSW 索引(`hnsw/`,设计 05 §3–§6)。
//!
//! 分层可导航小世界图:节点 id = 段内槽位下标,向量借用 [`IndexNode`] 的 `Arc`。
//! 构建期用确定性种子分配层级,使同一输入产生同一图(测试可复现);查询经
//! [`super::filtered`] 走过滤三档。

mod build;
mod link;
mod load;
mod model;
mod quant;
mod search;

#[cfg(test)]
mod tests;

// 仅为模块级 rustdoc 链接 [`IndexNode`] 可解析而引入,不参与代码路径。
#[allow(unused_imports)]
use crate::memory::index::IndexNode;

pub(crate) use build::BuildWithSlotsInput;
pub(crate) use load::HnswLoadInput;
pub(crate) use model::HnswIndex;
pub(crate) use search::QueryRef;
