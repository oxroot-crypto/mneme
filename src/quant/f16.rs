//! f16 量化(feature `quant-f16`):IEEE 754 half,每分量 2 字节(设计 08 §3)。
//!
//! 与 i8 不同,不需要段级 min/max 表、无跨段刻度问题、误差无偏;相对精度
//! `2^-11 ≈ 4.9e-4`,范围 `±65504`。粗排依旧两阶段:本模块只负责副本编解码与
//! 粗排点积,精排恒回退 f32 原向量(I12)。

use half::f16;

#[cfg(test)]
use crate::core::error::{MnemeError, Result};

/// 单分量字节数。
pub(crate) const BYTES_PER_ELEMENT: usize = 2;

/// 单行编码为小端 f16 码流(测试对照用;生产批量编码走 [`encode_row_into`])。
#[cfg(test)]
pub(crate) fn encode_row(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * BYTES_PER_ELEMENT);
    encode_row_into(&mut out, vector);
    out
}

/// 追加单行编码到 `out`(段级批量编码用,免每行一次 `Vec` 分配)。
pub(crate) fn encode_row_into(out: &mut Vec<u8>, vector: &[f32]) {
    for &value in vector {
        out.extend_from_slice(&f16::from_f32(value).to_bits().to_le_bytes());
    }
}

/// 单行解码;码流长度为奇数 → `Corrupted`(测试与误差界验证用)。
#[cfg(test)]
pub(crate) fn decode_row(codes: &[u8]) -> Result<Vec<f32>> {
    if !codes.len().is_multiple_of(BYTES_PER_ELEMENT) {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "f16 量化:码流长度非偶数".to_string(),
        });
    }
    Ok(codes
        .chunks_exact(BYTES_PER_ELEMENT)
        .map(|pair| f16::from_bits(u16::from_le_bytes([pair[0], pair[1]])).to_f32())
        .collect())
}

/// 单行粗排点积 `Σ q_i · f16(code_i)`(查询侧保持 f32);长度不符 →
/// `DimensionMismatch`(测试对照用;运行期走 [`coarse_dot_unchecked`])。
#[cfg(test)]
pub(crate) fn coarse_dot(query: &[f32], codes: &[u8]) -> Result<f32> {
    if codes.len() != query.len() * BYTES_PER_ELEMENT {
        return Err(MnemeError::DimensionMismatch {
            expected: query.len() as u32,
            got: codes.len() / BYTES_PER_ELEMENT,
        });
    }
    Ok(coarse_dot_unchecked(query, codes))
}

/// 不做长度校验的粗排点积(索引内部已保证「码流行数 × 维度 × 2」不变量)。
pub(crate) fn coarse_dot_unchecked(query: &[f32], codes: &[u8]) -> f32 {
    query
        .iter()
        .zip(codes.chunks_exact(BYTES_PER_ELEMENT))
        .map(|(value, pair)| {
            let code = f16::from_bits(u16::from_le_bytes([pair[0], pair[1]])).to_f32();
            value * code
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// f16 往返:相对误差不超过 `2^-11`(规格相对精度);测试值故意取
    /// 二进制不可精确表示者,确保真的发生舍入。
    #[test]
    fn roundtrip_within_half_precision() {
        let vector: Vec<f32> = vec![0.1, -0.3, 1.2345, -12.345, 999.9, 1.0 / 3.0, 4096.5];
        let restored = decode_row(&encode_row(&vector)).expect("decode_row");
        for (original, approx) in vector.iter().zip(&restored) {
            let bound = original.abs() * 2.0_f32.powi(-11) + 1e-6;
            assert!(
                (original - approx).abs() <= bound,
                "{original} → {approx} 超相对精度"
            );
        }
    }

    /// 极值往返:零(含 `-0.0`)、f16 最小 subnormal 与最小 normal、最大有限值
    /// 精确可表示,往返逐位一致。
    #[test]
    fn extremes_are_exactly_representable() {
        let exact: Vec<f32> = vec![
            0.0,
            -0.0,
            2.0_f32.powi(-24),
            2.0_f32.powi(-14),
            65504.0,
            -65504.0,
        ];
        let restored = decode_row(&encode_row(&exact)).expect("decode_row");
        for (original, approx) in exact.iter().zip(&restored) {
            assert_eq!(
                original.to_bits(),
                approx.to_bits(),
                "{original} 应精确往返,得到 {approx}"
            );
        }
    }

    /// 特殊值透传与溢出饱和:`NaN`/`±Inf` 保持,超出舍入中点约 `±65520` 的有限值
    /// 饱和为无穷(与 `half::f16::from_f32` 的 IEEE 语义一致)。
    #[test]
    fn special_values_are_preserved_and_overflow_saturates() {
        let vector = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.0e6, -1.0e6];
        let restored = decode_row(&encode_row(&vector)).expect("decode_row");
        assert!(restored[0].is_nan());
        assert_eq!(restored[1], f32::INFINITY);
        assert_eq!(restored[2], f32::NEG_INFINITY);
        assert_eq!(restored[3], f32::INFINITY, "超出范围的有限值饱和为 +Inf");
        assert_eq!(restored[4], f32::NEG_INFINITY, "负向溢出饱和为 -Inf");
    }

    /// 粗排点积与解码后 f32 点积一致。
    #[test]
    fn coarse_dot_matches_decoded_dot() {
        let query = [0.5_f32, -1.0, 2.0, 0.25];
        let row = [-0.75_f32, 1.5, 0.0, 3.0];
        let dot = coarse_dot(&query, &encode_row(&row)).expect("coarse_dot");
        let expected: f32 = query.iter().zip(&row).map(|(q, x)| q * x).sum();
        assert!((dot - expected).abs() < 1e-3, "{dot} vs {expected}");
    }

    /// 奇数长度码流、维度不符 → 结构化错误。
    #[test]
    fn malformed_codes_are_rejected() {
        assert!(matches!(
            decode_row(&[0_u8]),
            Err(MnemeError::Corrupted { .. })
        ));
        assert!(matches!(
            coarse_dot(&[1.0, 2.0], &[0_u8; 2]),
            Err(MnemeError::DimensionMismatch { .. })
        ));
    }

    proptest::proptest! {
        /// 任意有限 f32 的 f16 往返:相对误差上界 `2^-11`(规格相对精度),
        /// 不因取值分布漂移。
        #[test]
        fn roundtrip_error_is_bounded_prop(value in -6.5e4f32..6.5e4) {
            let restored = decode_row(&encode_row(&[value])).expect("decode_row");
            let bound = value.abs() * 2.0_f32.powi(-11) + 1e-6;
            proptest::prop_assert!(
                (value - restored[0]).abs() <= bound,
                "{value} → {} 超相对精度 {bound}",
                restored[0]
            );
        }
    }
}
