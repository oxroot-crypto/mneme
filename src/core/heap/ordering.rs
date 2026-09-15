//! TopK 的比较与堆序维护。
//!
//! `is_better` 在这里定义全序并集中操作计数;`sift_up` / `sift_down` 维持小根堆
//! 不变式(根 = 当前最差者)。

use crate::core::metric::Metric;

use super::entry::{Candidate, Entry};
use super::topk::TopK;

#[cfg(test)]
thread_local! {
    /// `is_better` 调用次数(操作计数,验证 push/merge/排序的复杂度界)。
    static COMPARES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 取出并清零 `is_better` 调用计数(仅测试)。
#[cfg(test)]
pub(crate) fn take_compares() -> usize {
    COMPARES.with(std::cell::Cell::take)
}

impl<T: Ord> TopK<T> {
    /// `a` 是否优于 `b`:先经 [`Metric::score_order`] 的全序比较(含 `NaN` 定序),
    /// 完全相等时比载荷升序。
    pub(super) fn is_better(metric: Metric, a: Candidate<'_, T>, b: Candidate<'_, T>) -> bool {
        #[cfg(test)]
        COMPARES.with(|count| count.set(count.get() + 1));
        match metric.score_order(a.score, b.score) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => a.payload < b.payload,
        }
    }

    /// 以引用视图看待堆内元素(不复制)。
    pub(super) fn candidate_view(entry: &Entry<T>) -> Candidate<'_, T> {
        Candidate {
            score: entry.score,
            payload: &entry.payload,
        }
    }

    /// 比较堆中两个下标处的元素。
    fn is_better_at(&self, a: usize, b: usize) -> bool {
        Self::is_better(
            self.metric,
            Self::candidate_view(&self.heap[a]),
            Self::candidate_view(&self.heap[b]),
        )
    }

    /// 上浮:若父节点优于子节点(违反"父 ≤ 子"),则交换,使更差者上移。
    pub(super) fn sift_up(&mut self, mut index: usize) {
        while index > 0 {
            let parent = (index - 1) / 2;
            if self.is_better_at(parent, index) {
                self.heap.swap(parent, index);
                index = parent;
            } else {
                break;
            }
        }
    }

    /// 下沉:选出本节点与子节点中最差者放到根位。
    pub(super) fn sift_down(&mut self, mut index: usize) {
        let len = self.heap.len();
        loop {
            let left = 2 * index + 1;
            let right = 2 * index + 2;
            let mut worst = index;
            if left < len && self.is_better_at(worst, left) {
                worst = left;
            }
            if right < len && self.is_better_at(worst, right) {
                worst = right;
            }
            if worst == index {
                break;
            }
            self.heap.swap(index, worst);
            index = worst;
        }
    }
}
