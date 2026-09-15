//! `heap` 模块的单元测试(有界 TopK 的排序、容量与复杂度界)。

use crate::core::metric::Metric;

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

/// FC-CORE-POST-004:`NaN` 分数与精排共用同一全序——`NaN` 恒排最后,
/// 不同 `top_k` 下代表元一致(不再出现 k=1/k=2 首名矛盾)。
#[test]
fn topk_orders_nan_consistently() {
    let mut one = TopK::new(1, Metric::Dot);
    one.push(1.0, 1_u32);
    one.push(f32::NAN, 2_u32);
    assert_eq!(one.into_sorted_vec(), vec![1], "NaN 不得挤掉有限候选");

    let mut two = TopK::new(2, Metric::Dot);
    two.push(f32::NAN, 2_u32);
    two.push(1.0, 1_u32);
    assert_eq!(two.into_sorted_vec(), vec![1, 2], "NaN 恒排最后");

    let mut negative_nan = TopK::new(1, Metric::Dot);
    negative_nan.push(-f32::NAN, 7_u32);
    negative_nan.push(0.5, 8_u32);
    assert_eq!(
        negative_nan.into_sorted_vec(),
        vec![8],
        "-NaN 也不得排在有限分之前"
    );
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
