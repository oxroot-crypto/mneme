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

/// 单行编码为小端 f16 码流。
pub(crate) fn encode_row(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * BYTES_PER_ELEMENT);
    for &value in vector {
        out.extend_from_slice(&f16::from_f32(value).to_bits().to_le_bytes());
    }
    out
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

    /// f16 往返:相对误差不超过 `2^-11`(规格相对精度)。
    #[test]
    fn roundtrip_within_half_precision() {
        let vector: Vec<f32> = (0..64).map(|index| (index as f32 - 32.0) * 0.25).collect();
        let restored = decode_row(&encode_row(&vector)).expect("decode_row");
        for (original, approx) in vector.iter().zip(&restored) {
            let bound = original.abs() * 2.0_f32.powi(-11) + 1e-6;
            assert!(
                (original - approx).abs() <= bound,
                "{original} → {approx} 超相对精度"
            );
        }
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
}
