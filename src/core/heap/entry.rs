//! TopK 堆内的候选条目与比较视图。
//!
//! `Entry` 是堆中实际存储的元素;`Candidate` 是只读比较视图,聚合分数与载荷引用,
//! 供比较逻辑收敛参数个数。

use crate::core::metric::Score;

/// 一个候选:分数与载荷。
#[derive(Debug)]
pub(super) struct Entry<T> {
    pub(super) score: Score,
    pub(super) payload: T,
}

/// 堆内候选的比较视图:聚合分数与载荷引用,收敛 `is_better` 的参数个数。
pub(super) struct Candidate<'a, T> {
    pub(super) score: Score,
    pub(super) payload: &'a T,
}
