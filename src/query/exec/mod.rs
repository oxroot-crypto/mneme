//! `SearchBuilder` 的检索执行流水线(设计 06 §5)。
//!
//! 管线顺序固定:**过滤先行 → 双通道各取 top-k → 融合 → (可选)关系扩展 →
//! 综合打分 → 结果去重/多样性 → 精排钩子**。单通道(仅向量/仅文本)直接取
//! `top_k`;双通道各取 `2k` 再融合取 `k`,减少截断遗憾。
//!
//! 本模块承载 [`SearchBuilder::execute`](crate::SearchBuilder::execute) 与各内部
//! 阶段,使 L4 → L1 的依赖方向成立(L1 只定义类型与 setter)。

mod channels;
mod entry;
mod post;
mod validate;
