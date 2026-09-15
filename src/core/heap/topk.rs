//! `TopK` 有界堆的定义与公开 API。
//!
//! 比较与堆序维护见父模块的 `ordering` 子模块。

use crate::core::metric::{Metric, Score};

use super::entry::{Candidate, Entry};

/// `TopK` 初始预分配容量的上限:避免 `k` 极大时一次性占用过多内存。
const TOPK_MAX_PREALLOC: usize = 1024;

/// 只保留最优 k 个元素的有界堆。
///
/// 载荷类型 `T` 需实现 [`Ord`]:同分时以载荷升序作为稳定的次级排序键。
#[derive(Debug)]
pub struct TopK<T: Ord> {
    pub(super) k: usize,
    pub(super) metric: Metric,
    pub(super) heap: Vec<Entry<T>>,
}

impl<T: Ord> TopK<T> {
    /// 构造一个容量为 `k` 的堆。
    ///
    /// # Arguments
    ///
    /// * `k` - 保留的最优元素个数;`0` 表示不保留任何元素。
    /// * `metric` - 决定"更优"方向的度量。
    ///
    /// # Returns
    ///
    /// 一个空的有界堆,容量为 `k`。
    ///
    /// # Examples
    ///
    /// ```
    /// use mneme::{Metric, TopK};
    ///
    /// let top: TopK<u32> = TopK::new(3, Metric::Dot);
    /// assert_eq!(top.capacity(), 3);
    /// assert!(top.is_empty());
    /// ```
    pub fn new(k: usize, metric: Metric) -> Self {
        Self {
            k,
            metric,
            heap: Vec::with_capacity(k.min(TOPK_MAX_PREALLOC)),
        }
    }

    /// 当前保留的元素个数。
    ///
    /// # Returns
    ///
    /// 已保留个数,恒 ≤ 容量 `k`。
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// 是否未保留任何元素。
    ///
    /// # Returns
    ///
    /// 未保留任何元素时返回 `true`。
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// 容量 `k`。
    ///
    /// # Returns
    ///
    /// 构造时传入的保留个数 `k`(语义容量,非内部缓冲区分配量)。
    pub fn capacity(&self) -> usize {
        self.k
    }

    /// 内部缓冲区的实际分配容量(单测验证 `new` 的预分配上界,不属公开 API)。
    #[cfg(test)]
    pub(super) fn allocated(&self) -> usize {
        self.heap.capacity()
    }

    /// 推入一个候选;超出容量且不优于当前最差者时直接丢弃。
    ///
    /// # Arguments
    ///
    /// * `score` - 该候选的原始分数。
    /// * `payload` - 随分数保留的载荷(如行标识)。
    ///
    /// # Examples
    ///
    /// ```
    /// use mneme::{Metric, TopK};
    ///
    /// let mut top = TopK::new(2, Metric::Dot);
    /// top.push(1.0, 1_u32);
    /// top.push(3.0, 3_u32);
    /// top.push(2.0, 2_u32);
    /// assert_eq!(top.into_sorted_vec(), vec![3, 2]);
    /// ```
    pub fn push(&mut self, score: Score, payload: T) {
        if self.k == 0 {
            return;
        }
        if self.heap.len() < self.k {
            self.heap.push(Entry { score, payload });
            let last = self.heap.len() - 1;
            self.sift_up(last);
            return;
        }
        // 根是当前最差者;新候选必须更优才替换。
        let root = &self.heap[0];
        let worse = Candidate {
            score: root.score,
            payload: &root.payload,
        };
        if !Self::is_better(
            self.metric,
            Candidate {
                score,
                payload: &payload,
            },
            worse,
        ) {
            return;
        }
        self.heap[0] = Entry { score, payload };
        self.sift_down(0);
    }

    /// 归并另一个同容量堆。
    ///
    /// 结果 ≡ 把两者保留的元素按序 `push` 回本堆;用于并行扫描后的 k 路归并。
    ///
    /// # Arguments
    ///
    /// * `other` - 被归并的同容量堆;归并后其保留元素被移入本堆并被消费。
    ///
    /// # Examples
    ///
    /// ```
    /// use mneme::{Metric, TopK};
    ///
    /// let mut left = TopK::new(2, Metric::Dot);
    /// left.push(1.0, 1_u32);
    /// let mut right = TopK::new(2, Metric::Dot);
    /// right.push(5.0, 5_u32);
    /// left.merge(right);
    /// assert_eq!(left.into_sorted_vec(), vec![5, 1]);
    /// ```
    pub fn merge(&mut self, other: Self) {
        for entry in other.heap {
            self.push(entry.score, entry.payload);
        }
    }

    /// 消费堆,按"最优在前"返回载荷。
    ///
    /// 同分按载荷升序。
    ///
    /// # Returns
    ///
    /// 按最优在前排序的载荷列表;同分按载荷升序。堆为空时返回空 `Vec`。
    ///
    /// # Examples
    ///
    /// ```
    /// use mneme::{Metric, TopK};
    ///
    /// let mut top = TopK::new(2, Metric::Euclidean);
    /// top.push(0.5, 5_u32);
    /// top.push(0.1, 1_u32);
    /// assert_eq!(top.into_sorted_vec(), vec![1, 5]);
    /// ```
    pub fn into_sorted_vec(mut self) -> Vec<T> {
        let metric = self.metric;
        self.heap.sort_by(|a, b| {
            if Self::is_better(metric, Self::candidate_view(a), Self::candidate_view(b)) {
                std::cmp::Ordering::Less
            } else if Self::is_better(metric, Self::candidate_view(b), Self::candidate_view(a)) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        });
        self.heap.into_iter().map(|entry| entry.payload).collect()
    }
}
