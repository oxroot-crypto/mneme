//! L0 TopK 有界堆。
//!
//! 从 N 个分数里挑最优的 k 个,只需容纳 k 个元素的内存,而不是给 N 个分数排序。
//! 维护一个小根堆(根 = 当前最差,即"守门员"):新分数打不过守门员就直接丢弃,
//! 打得过就挤掉守门员再重新站岗。全程方向由 [`Metric::better`] 决定,语义始终是
//! "保留最优的 k 个"。
//!
//! 同分时按载荷升序稳定排序,使结果与插入顺序无关、可确定性复现;`merge` 支持并行
//! 扫描时的 k 路归并,代价 $O(k \log k)$,与 N 无关。

use crate::core::metric::{Metric, Score};

/// 一个候选:分数与载荷。
#[derive(Debug)]
struct Entry<T> {
    score: Score,
    payload: T,
}

/// 堆内候选的比较视图:聚合分数与载荷引用,收敛 `is_better` 的参数个数。
struct Candidate<'a, T> {
    score: Score,
    payload: &'a T,
}

/// `TopK` 初始预分配容量的上限:避免 `k` 极大时一次性占用过多内存。
const TOPK_MAX_PREALLOC: usize = 1024;

#[cfg(test)]
thread_local! {
    /// `is_better` 调用次数(操作计数,验证 push/merge/排序的复杂度界)。
    static COMPARES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 取出并清零 `is_better` 调用计数(仅测试)。
#[cfg(test)]
fn take_compares() -> usize {
    COMPARES.with(std::cell::Cell::take)
}

/// 只保留最优 k 个元素的有界堆。
///
/// 载荷类型 `T` 需实现 [`Ord`]:同分时以载荷升序作为稳定的次级排序键。
#[derive(Debug)]
pub struct TopK<T: Ord> {
    k: usize,
    metric: Metric,
    heap: Vec<Entry<T>>,
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
    fn allocated(&self) -> usize {
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

    /// `a` 是否优于 `b`:先比分数方向,同分比载荷升序。
    fn is_better(metric: Metric, a: Candidate<'_, T>, b: Candidate<'_, T>) -> bool {
        #[cfg(test)]
        COMPARES.with(|count| count.set(count.get() + 1));
        if metric.better(a.score, b.score) {
            true
        } else if metric.better(b.score, a.score) {
            false
        } else {
            a.payload < b.payload
        }
    }

    /// 以引用视图看待堆内元素(不复制)。
    fn candidate_view(entry: &Entry<T>) -> Candidate<'_, T> {
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
    fn sift_up(&mut self, mut index: usize) {
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
    fn sift_down(&mut self, mut index: usize) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_top_k_by_dot_direction() {
        let mut top = TopK::new(3, Metric::Dot);
        for (score, id) in [
            (5.0, 0_u32),
            (9.0, 1),
            (1.0, 2),
            (7.0, 3),
            (3.0, 4),
            (8.0, 5),
        ] {
            top.push(score, id);
        }
        assert_eq!(top.len(), 3);
        assert_eq!(top.into_sorted_vec(), vec![1, 5, 3]);
    }

    #[test]
    fn keeps_smallest_for_euclidean() {
        let mut top = TopK::new(2, Metric::Euclidean);
        for (distance, id) in [(0.9_f32, 0_u32), (0.1, 1), (0.5, 2)] {
            top.push(distance, id);
        }
        assert_eq!(top.into_sorted_vec(), vec![1, 2]);
    }

    #[test]
    fn ties_break_by_payload_ascending() {
        let mut top = TopK::new(3, Metric::Dot);
        top.push(1.0, 9_u32);
        top.push(1.0, 2_u32);
        top.push(1.0, 5_u32);
        assert_eq!(top.into_sorted_vec(), vec![2, 5, 9]);
    }

    #[test]
    fn zero_capacity_retains_nothing() {
        let mut top = TopK::new(0, Metric::Dot);
        top.push(1.0, 1_u32);
        assert!(top.is_empty());
        assert_eq!(top.capacity(), 0);
        assert!(top.into_sorted_vec().is_empty());
    }

    #[test]
    fn fewer_than_k_returns_all_sorted() {
        let mut top = TopK::new(10, Metric::Dot);
        top.push(2.0, 2_u32);
        top.push(1.0, 1_u32);
        top.push(3.0, 3_u32);
        assert_eq!(top.into_sorted_vec(), vec![3, 2, 1]);
    }

    /// FC-CORE-CPLX-003:`TopK::new` 预分配 ≤ `min(k, 1024)`(上界/紧邻越界点/零).
    #[test]
    fn topk_prealloc_bounded_by_min_k_1024() {
        assert_eq!(TopK::<u32>::new(0, Metric::Dot).allocated(), 0);
        assert_eq!(TopK::<u32>::new(10, Metric::Dot).allocated(), 10);
        assert_eq!(TopK::<u32>::new(1_024, Metric::Dot).allocated(), 1_024);
        assert_eq!(TopK::<u32>::new(1_025, Metric::Dot).allocated(), 1_024);
        assert_eq!(TopK::<u32>::new(4_096, Metric::Dot).allocated(), 1_024);
    }

    /// FC-CORE-CPLX-003:未满上浮 ≤ ⌈log₂ n⌉;已满拒绝恰 1 次比较;
    /// 已满替换 ≤ 1 + 2⌈log₂ k⌉(与扫描规模无关)。
    #[test]
    fn topk_push_outside_k_costs_constant_or_log_k() {
        let k = 64_usize;
        let mut top = TopK::new(k, Metric::Dot);
        for i in 0..k {
            let _ = take_compares();
            top.push(i as f32, i as u32);
            let compares = take_compares();
            let bound = (i.max(1) as f64).log2().ceil() as usize + 1;
            assert!(
                compares <= bound,
                "第 {i} 次未满 push 比较 {compares} 次,超上界 {bound}"
            );
        }

        // 已满且不如守门员:O(1) 拒绝,恰好 1 次比较。
        let _ = take_compares();
        top.push(-1.0, u32::MAX);
        assert_eq!(take_compares(), 1, "已满拒绝路径必须恰 1 次比较");

        // 已满且优于守门员:1 次根比较 + 下沉 ≤ 2⌈log₂ k⌉。
        let _ = take_compares();
        top.push(1_000.0, u32::MAX);
        let replaces = take_compares();
        let bound = 1 + 2 * (k as f64).log2().ceil() as usize;
        assert!(
            replaces <= bound,
            "已满替换比较 {replaces} 次,超上界 {bound}"
        );
    }

    /// FC-CORE-CPLX-004:`merge` / `into_sorted_vec` 只触及 ≤ `k` 个元素,
    /// 比较次数以 $k\log k$ 为界,与扫描规模 $N$ 无关。
    #[test]
    fn topk_merge_and_sort_cost_bounded_by_k() {
        let k = 64_usize;
        let mut left = TopK::new(k, Metric::Dot);
        let mut right = TopK::new(k, Metric::Dot);
        for i in 0..(k * 16) {
            left.push((i % 997) as f32, i as u32);
            right.push(((i * 7) % 997) as f32, (i + 1) as u32);
        }

        let _ = take_compares();
        left.merge(right);
        let merge_compares = take_compares();
        let merge_bound = k * (1 + 2 * (k as f64).log2().ceil() as usize);
        assert!(
            merge_compares <= merge_bound,
            "merge 比较 {merge_compares} 次,超上界 {merge_bound}"
        );

        let _ = take_compares();
        let sorted = left.into_sorted_vec();
        let sort_compares = take_compares();
        assert_eq!(sorted.len(), k);
        let sort_bound = 2 * k * (k as f64).log2().ceil() as usize;
        assert!(
            sort_compares <= sort_bound,
            "排序比较 {sort_compares} 次,超上界 {sort_bound}"
        );
    }
}
