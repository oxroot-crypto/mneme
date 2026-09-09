//! L0 原语层形式化契约验收测试。
//!
//! 本文件覆盖以下 `FC-CORE-*` 契约(见 `docs/spec/contracts.md`):
//!
//! * FC-CORE-PRE-001  —— 维度闭区间 `[1, 65536]`
//! * FC-CORE-POST-001 —— `Metric::score` 三分支数学映射
//! * FC-CORE-POST-002 —— `Metric::better` 统一方向
//! * FC-CORE-POST-003 —— `Metric::needs_norm`
//! * FC-CORE-POST-004 —— `TopK` 有界保留 / `merge` 等价
//! * FC-CORE-POST-005 —— varint 往返且最小编码
//! * FC-CORE-POST-006 —— `meta` 路径与类型访问器
//! * FC-CORE-POST-007 —— `RelationKind` 内置编号
//! * FC-CORE-INV-001  —— `simd::dot` ≡ 标量参考
//! * FC-CORE-INV-002  —— 公开 API 不 panic
//! * FC-CORE-ERR-001  —— varint 畸形输入结构化报错
//! * FC-CORE-ERR-002  —— 余弦零向量返回 0

use std::cmp::Ordering;

use mneme::meta::{as_bool, as_f64, as_i64, as_str, as_ts, get_path};
use mneme::simd::{dot, dot_scalar};
use mneme::varint::{decode_u32, decode_u64, encode_u32, encode_u64};
use mneme::{Dimension, Metric, RelationKind, TopK, json};
use proptest::prelude::*;

/// 参照实现:按"最优在前、同分载荷升序"排序后取前 k 个。
fn reference_topk(entries: &[(f32, u32)], k: usize, metric: Metric) -> Vec<u32> {
    let mut ordered = entries.to_vec();
    ordered.sort_by(|a, b| {
        if metric.better(a.0, b.0) {
            Ordering::Less
        } else if metric.better(b.0, a.0) {
            Ordering::Greater
        } else {
            a.1.cmp(&b.1)
        }
    });
    ordered.truncate(k);
    ordered.into_iter().map(|(_, id)| id).collect()
}

/// FC-CORE-PRE-001
#[test]
fn dimension_bounds() {
    assert_eq!(Dimension::new(1).expect("下界合法").get(), 1);
    assert_eq!(Dimension::new(65_536).expect("上界合法").get(), 65_536);
    assert!(Dimension::new(0).is_err(), "下界紧邻越界点必须拒绝");
    assert!(Dimension::new(65_537).is_err(), "上界紧邻越界点必须拒绝");
    assert!(Dimension::new(u32::MAX).is_err());
}

/// FC-CORE-POST-001
#[test]
fn metric_score_mapping() {
    let a = [1.0_f32, 2.0, 3.0];
    let b = [4.0_f32, 5.0, 6.0];
    let a_norm: f32 = 1.0 + 4.0 + 9.0;
    let b_norm: f32 = 16.0 + 25.0 + 36.0;
    let inner = 32.0_f32;

    assert!((Metric::Dot.score(&a, &b, 0.0, 0.0) - inner).abs() < 1e-6);
    let expected_cos = inner / (a_norm * b_norm).sqrt();
    assert!((Metric::Cosine.score(&a, &b, a_norm, b_norm) - expected_cos).abs() < 1e-6);
    let expected_euclid = a_norm + b_norm - 2.0 * inner;
    assert!((Metric::Euclidean.score(&a, &b, a_norm, b_norm) - expected_euclid).abs() < 1e-4);
}

/// FC-CORE-POST-002
#[test]
fn metric_better_direction() {
    assert!(Metric::Cosine.better(0.9, 0.1));
    assert!(!Metric::Cosine.better(0.1, 0.9));
    assert!(Metric::Dot.better(2.0, 1.0));
    assert!(Metric::Euclidean.better(1.0, 2.0));
    assert!(!Metric::Euclidean.better(2.0, 1.0));
}

/// FC-CORE-POST-003
#[test]
fn metric_needs_norm() {
    assert!(!Metric::Dot.needs_norm());
    assert!(Metric::Cosine.needs_norm());
    assert!(Metric::Euclidean.needs_norm());
}

/// FC-CORE-POST-006
#[test]
fn meta_accessors() {
    let value = json!({"a": {"b": "text"}, "n": 7, "f": 2.5, "flag": false});
    assert_eq!(get_path(&value, "a.b").and_then(as_str), Some("text"));
    assert_eq!(get_path(&value, "n").and_then(as_i64), Some(7));
    assert_eq!(get_path(&value, "f").and_then(as_f64), Some(2.5));
    assert_eq!(get_path(&value, "flag").and_then(as_bool), Some(false));
    assert_eq!(get_path(&value, "n").and_then(as_ts), Some(7));
    assert!(get_path(&value, "a.missing").is_none());
    assert!(get_path(&value, "n.deeper").is_none());
}

/// FC-CORE-POST-007
#[test]
fn relation_kind_builtins() {
    assert_eq!(RelationKind::DERIVED_FROM.0, 0);
    assert_eq!(RelationKind::SUPPORTS.0, 1);
    assert_eq!(RelationKind::CONTRADICTS.0, 2);
    assert_eq!(RelationKind::RELATED.0, 3);
    assert_eq!(RelationKind::FIRST_CUSTOM, 16);
}

/// FC-CORE-ERR-001
#[test]
fn varint_malformed() {
    assert!(decode_u64(&[0x80]).is_err(), "截断必须报错");
    assert!(decode_u64(&[]).is_err(), "空输入必须报错");
    assert!(decode_u64(&[0x80; 11]).is_err(), "超长必须报错");
    assert!(decode_u64(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02]).is_err());
    assert!(decode_u32(&[0x80, 0x80, 0x80, 0x80, 0x10]).is_err());
}

/// FC-CORE-ERR-002
#[test]
fn cosine_zero_vector() {
    let zero = [0.0_f32, 0.0, 0.0];
    let other = [1.0_f32, 2.0, 3.0];
    let score = Metric::Cosine.score(&zero, &other, 0.0, 14.0);
    assert!(score.is_finite(), "零向量不得产生 NaN/Inf");
    assert_eq!(score, 0.0);
}

/// FC-CORE-INV-002(确定性极值输入不 panic)
#[test]
fn no_panic() {
    // 空向量、零向量、单元素、超长向量。
    assert_eq!(dot(&[], &[]), 0.0);
    assert_eq!(Metric::Dot.score(&[], &[], 0.0, 0.0), 0.0);
    assert_eq!(Metric::Cosine.score(&[0.0], &[0.0], 0.0, 0.0), 0.0);

    let long_a = vec![0.5_f32; 10_000];
    let long_b = vec![-0.5_f32; 10_000];
    let _ = Metric::Euclidean.score(&long_a, &long_b, 2500.0, 2500.0);

    // varint 任意字节串不得 panic。
    for len in 0..32_usize {
        let bytes = vec![0xAB_u8; len];
        let _ = decode_u64(&bytes);
        let _ = decode_u32(&bytes);
    }
}

proptest! {
    /// FC-CORE-POST-004
    #[test]
    fn topk_equivalence(
        entries in prop::collection::vec((0.0f32..1000.0, any::<u32>()), 0..200),
        k in 0usize..24,
    ) {
        let metric = Metric::Dot;
        let expected = reference_topk(&entries, k, metric);

        let mut all = TopK::new(k, metric);
        for &(score, id) in &entries {
            all.push(score, id);
        }
        prop_assert_eq!(all.into_sorted_vec(), expected.clone());

        // merge 两个分块 ≡ 顺序 push 全部元素。
        let mid = entries.len() / 2;
        let mut left = TopK::new(k, metric);
        let mut right = TopK::new(k, metric);
        for &(score, id) in &entries[..mid] {
            left.push(score, id);
        }
        for &(score, id) in &entries[mid..] {
            right.push(score, id);
        }
        left.merge(right);
        prop_assert_eq!(left.into_sorted_vec(), expected);
    }

    /// FC-CORE-POST-005
    #[test]
    fn varint_roundtrip_minimal(value in any::<u64>()) {
        let mut buf = Vec::new();
        encode_u64(value, &mut buf);
        prop_assert_eq!(decode_u64(&buf).unwrap(), (value, buf.len()));

        // 最小编码:字节数 == ceil(有效位数 / 7)。
        let significant_bits = (64 - value.leading_zeros()).max(1);
        let expected_len = significant_bits.div_ceil(7) as usize;
        prop_assert_eq!(buf.len(), expected_len);

        let value32 = value as u32;
        let mut buf32 = Vec::new();
        encode_u32(value32, &mut buf32);
        prop_assert_eq!(decode_u32(&buf32).unwrap(), (value32, buf32.len()));
    }

    /// FC-CORE-INV-001
    #[test]
    fn dot_matches_scalar_reference(
        a in prop::collection::vec(-10.0f32..10.0, 0..300),
        b in prop::collection::vec(-10.0f32..10.0, 0..300),
    ) {
        let len = a.len().min(b.len());
        let a = &a[..len];
        let b = &b[..len];
        let vectorized = dot(a, b);
        let scalar = dot_scalar(a, b);
        let tolerance = 1e-4 * (1.0 + scalar.abs());
        prop_assert!(
            (vectorized - scalar).abs() <= tolerance,
            "dot={vectorized} scalar={scalar}"
        );
    }
}
