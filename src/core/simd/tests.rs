//! `simd` 模块的单元测试(点积等价性、尾块与操作计数)。

use super::*;

#[test]
fn empty_slices_produce_zero() {
    assert_eq!(dot(&[], &[]), 0.0);
    assert_eq!(dot_scalar(&[], &[]), 0.0);
}

#[test]
fn dot_matches_hand_computed_value() {
    let a = [1.0_f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
    let b = [1.0_f32, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
    assert_eq!(dot(&a, &b), 45.0);
}

#[test]
fn dot_handles_tail_lanes() {
    for len in 0..=17_usize {
        let a: Vec<f32> = (0..len).map(|i| i as f32).collect();
        let b: Vec<f32> = (0..len).map(|i| (i + 1) as f32).collect();
        assert!((dot(&a, &b) - dot_scalar(&a, &b)).abs() < 1e-4);
    }
}

/// FC-CORE-CPLX-001:逐元素乘加次数恰为 `min(len)`(时间 $O(d)$ 的操作计数;
/// `dot` 为同复杂度的 SIMD 实现,等价性由 FC-CORE-INV-001 保证)。
#[test]
fn dot_scalar_per_element_cost_is_linear() {
    for len in [0_usize, 1, 7, 64, 1_024] {
        let a = vec![1.0_f32; len];
        let b = vec![2.0_f32; len];
        MUL_ADDS.with(|count| count.set(0));
        let result = dot_scalar(&a, &b);
        assert_eq!(result, 2.0 * len as f32);
        assert_eq!(take_mul_adds(), len, "乘加次数必须恰为 min(len)");
    }
}

/// `dot_u8_f32`(L6 i8 粗排内核)与标量参考实现在所有尾块长度上一致。
#[test]
fn dot_u8_f32_matches_scalar_reference() {
    for len in [0_usize, 1, 7, 8, 9, 31, 64, 257] {
        let codes: Vec<u8> = (0..len).map(|index| (index * 37 % 256) as u8).collect();
        let weights: Vec<f32> = (0..len).map(|index| index as f32 * 0.25 - 3.0).collect();
        let expected = dot_u8_f32_scalar(&codes, &weights);
        let got = dot_u8_f32(&codes, &weights);
        assert!(
            (got - expected).abs() <= expected.abs().max(1.0) * 1e-4,
            "len={len}: {got} vs {expected}"
        );
    }
}

/// 码值取满 `[0, 255]` 全域时仍与标量一致。
#[test]
fn dot_u8_f32_handles_full_code_range() {
    let codes: Vec<u8> = (0..=255_u8).collect();
    let weights: Vec<f32> = (0..256).map(|index| 1.0 - index as f32 / 128.0).collect();
    let expected = dot_u8_f32_scalar(&codes, &weights);
    let got = dot_u8_f32(&codes, &weights);
    assert!((got - expected).abs() <= expected.abs().max(1.0) * 1e-4);
}
