//! L0 TopK 有界堆。
//!
//! 从 N 个分数里挑最优的 k 个,只需容纳 k 个元素的内存,而不是给 N 个分数排序。
//! 维护一个小根堆(根 = 当前最差,即"守门员"):新分数打不过守门员就直接丢弃,
//! 打得过就挤掉守门员再重新站岗。全程方向由
//! [`Metric::better`](crate::core::metric::Metric::better) 决定,语义始终是
//! "保留最优的 k 个"。
//!
//! 同分时按载荷升序稳定排序,使结果与插入顺序无关、可确定性复现;`merge` 支持并行
//! 扫描时的 k 路归并,代价 $O(k \log k)$,与 N 无关。
//!
//! # 子模块
//!
//! * `entry` —— 候选条目与比较视图。
//! * `ordering` —— 比较与堆序维护(上浮/下沉)。
//! * `topk` —— [`TopK`] 定义与公开 API。

mod entry;
mod ordering;
mod topk;

#[cfg(test)]
mod tests;

pub use topk::TopK;

#[cfg(test)]
pub(crate) use ordering::take_compares;
