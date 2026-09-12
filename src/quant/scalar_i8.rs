//! i8 标量量化:段级逐维 `(v_min, v_max)`,编解码与粗排点积(设计 08 §2)。
//!
//! 编码公式(每维独立):
//!
//! ```text
//! Δ_i = (v_max_i − v_min_i) / 255
//! q_i = round((x_i − v_min_i) / Δ_i) ∈ [0, 255]
//! x̂_i = v_min_i + q_i · Δ_i
//! ```
//!
//! 误差上界 `|x_i − x̂_i| ≤ Δ_i/2`(舍入误差);`v_max_i = v_min_i` 的退化维取
//! `Δ_i = 0`、编码恒 0、解码恒 `v_min_i`(FC-QUANT-POST-001)。
//!
//! 粗排点积保持查询向量为 f32(只量化库侧):给定段级参数先算查询权重
//! `w_i = q_i · Δ_i` 与偏置 `b = Σ q_i · v_min_i`,每行只需
//! `score = b + Σ code_i · w_i`,读侧带宽恒为 `d` 字节/行(设计 08 §2.3)。

use crate::core::error::{MnemeError, Result};
use crate::core::simd;

/// i8 码位上限:`2^8 − 1`。
const MAX_CODE: f32 = 255.0;

/// 段级逐维量化参数。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct I8Params {
    /// 每维下界 `v_min`。
    mins: Vec<f32>,
    /// 每维上界 `v_max`。
    maxs: Vec<f32>,
}

impl I8Params {
    /// 由交错 `(v_min, v_max)` 表构造(磁盘 qvec 区格式,FC-QUANT-POST-002)。
    ///
    /// # Errors
    /// 表长不是 `2 × dimension`、含非有限值或 `v_min > v_max` 时返回
    /// [`MnemeError::Corrupted`]。
    pub(crate) fn from_table(table: &[f32], dimension: usize) -> Result<Self> {
        if table.len() != dimension * 2 {
            return Err(corrupt("i8 参数表长度与维度不符"));
        }
        let mut mins = Vec::with_capacity(dimension);
        let mut maxs = Vec::with_capacity(dimension);
        for (index, pair) in table.chunks_exact(2).enumerate() {
            let (min, max) = (pair[0], pair[1]);
            if !min.is_finite() || !max.is_finite() || min > max {
                return Err(corrupt(&format!("i8 参数表第 {index} 维非法")));
            }
            mins.push(min);
            maxs.push(max);
        }
        Ok(Self { mins, maxs })
    }

    /// 交错 `(v_min, v_max)` 表(磁盘编码用;与解析输入逐位一致)。
    pub(crate) fn table(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.dimension() * 2);
        for (min, max) in self.mins.iter().zip(&self.maxs) {
            out.push(*min);
            out.push(*max);
        }
        out
    }

    /// 维度。
    pub(crate) fn dimension(&self) -> usize {
        self.mins.len()
    }

    /// 第 `dim` 维下界。
    pub(crate) fn min(&self, dim: usize) -> f32 {
        self.mins[dim]
    }

    /// 第 `dim` 维步长 `Δ`;上界等于下界的退化维返回 0。
    pub(crate) fn delta(&self, dim: usize) -> f32 {
        (self.maxs[dim] - self.mins[dim]) / MAX_CODE
    }
}

/// 由段内全部向量建立逐维参数(段内统计,非对称区间)。
///
/// # Errors
/// 向量为空、维度不符或含非有限分量时返回结构化错误。
pub(crate) fn build_params(vectors: &[&[f32]], dimension: usize) -> Result<I8Params> {
    if vectors.is_empty() {
        return Err(MnemeError::Inconsistent {
            reason: "i8 量化至少需要一行向量",
        });
    }
    let mut mins = vec![f32::INFINITY; dimension];
    let mut maxs = vec![f32::NEG_INFINITY; dimension];
    for vector in vectors {
        if vector.len() != dimension {
            return Err(MnemeError::DimensionMismatch {
                expected: dimension as u32,
                got: vector.len(),
            });
        }
        for (dim, &value) in vector.iter().enumerate() {
            if !value.is_finite() {
                return Err(MnemeError::NonFinite);
            }
            mins[dim] = mins[dim].min(value);
            maxs[dim] = maxs[dim].max(value);
        }
    }
    Ok(I8Params { mins, maxs })
}

/// 单行编码:`q_i = round((x_i − v_min_i)/Δ_i)` 钳到 `[0, 255]`;退化维恒 0。
pub(crate) fn encode_row(vector: &[f32], params: &I8Params) -> Vec<u8> {
    debug_assert_eq!(vector.len(), params.dimension());
    vector
        .iter()
        .enumerate()
        .map(|(dim, &value)| {
            let delta = params.delta(dim);
            if delta == 0.0 {
                return 0_u8;
            }
            let code = ((value - params.min(dim)) / delta).round();
            code.clamp(0.0, MAX_CODE) as u8
        })
        .collect()
}

/// 单行解码 `x̂_i = v_min_i + q_i · Δ_i`(误差界验证与测试对照用)。
#[cfg(test)]
pub(crate) fn decode_row(codes: &[u8], params: &I8Params) -> Vec<f32> {
    debug_assert_eq!(codes.len(), params.dimension());
    codes
        .iter()
        .enumerate()
        .map(|(dim, &code)| params.min(dim) + f32::from(code) * params.delta(dim))
        .collect()
}

/// 查询侧预计算:每维权重 `w_i = q_i · Δ_i` 与偏置 `b = Σ q_i · v_min_i`。
///
/// 同一段内每行粗排只需一次 `b + Σ code_i · w_i`(读取 `d` 字节)。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Query {
    /// 每维权重。
    weights: Vec<f32>,
    /// 查询与参数下界的内积(标量偏置)。
    bias: f32,
}

impl Query {
    /// 由 f32 查询向量与段级参数预计算。
    ///
    /// # Errors
    /// 维度不符 → [`MnemeError::DimensionMismatch`];分量非有限 →
    /// [`MnemeError::NonFinite`]。
    pub(crate) fn new(query: &[f32], params: &I8Params) -> Result<Self> {
        if query.len() != params.dimension() {
            return Err(MnemeError::DimensionMismatch {
                expected: params.dimension() as u32,
                got: query.len(),
            });
        }
        let mut weights = Vec::with_capacity(query.len());
        let mut bias = 0.0_f32;
        for (dim, &value) in query.iter().enumerate() {
            if !value.is_finite() {
                return Err(MnemeError::NonFinite);
            }
            weights.push(value * params.delta(dim));
            bias += value * params.min(dim);
        }
        Ok(Self { weights, bias })
    }

    /// 单行粗排分 `b + Σ code_i · w_i`。
    pub(crate) fn score(&self, codes: &[u8]) -> f32 {
        self.bias + simd::dot_u8_f32(codes, &self.weights)
    }
}

/// 构造 qvec 区损坏错误。
fn corrupt(reason: &str) -> MnemeError {
    MnemeError::Corrupted {
        segment: None,
        reason: format!("i8 量化: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_vectors() -> Vec<Vec<f32>> {
        (0..8)
            .map(|row| {
                (0..5)
                    .map(|col| ((row * 5 + col) as f32 - 17.0) * 0.37)
                    .collect()
            })
            .collect()
    }

    fn sample_params() -> I8Params {
        let vectors = sample_vectors();
        let refs: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
        build_params(&refs, 5).expect("build_params")
    }

    /// FC-QUANT-POST-001:逐维误差上界 `|x − x̂| ≤ Δ/2`。
    #[test]
    fn encode_error_within_half_delta() {
        let params = sample_params();
        for vector in sample_vectors() {
            let codes = encode_row(&vector, &params);
            let restored = decode_row(&codes, &params);
            for (dim, (&original, &approx)) in vector.iter().zip(&restored).enumerate() {
                let bound = params.delta(dim) / 2.0;
                assert!(
                    (original - approx).abs() <= bound + 1e-6,
                    "第 {dim} 维误差 {} 超过上界 {bound}",
                    (original - approx).abs()
                );
            }
        }
    }

    /// FC-QUANT-POST-002(片级):参数表 `(v_min, v_max)` 往返逐位一致。
    #[test]
    fn table_roundtrip_is_bit_exact() {
        let params = sample_params();
        let table = params.table();
        let parsed = I8Params::from_table(&table, 5).expect("from_table");
        assert_eq!(parsed.table(), table);
        assert_eq!(parsed, params);
    }

    /// 退化维(上界 = 下界):编码恒 0、解码恒该常量,误差为 0。
    #[test]
    fn degenerate_dimension_encodes_zero() {
        let vectors = vec![vec![2.5_f32, 1.0], vec![2.5, 3.0]];
        let refs: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
        let params = build_params(&refs, 2).expect("build_params");
        assert_eq!(params.delta(0), 0.0);
        for vector in &vectors {
            let codes = encode_row(vector, &params);
            assert_eq!(codes[0], 0);
            let restored = decode_row(&codes, &params);
            assert_eq!(restored[0], 2.5);
        }
    }

    /// 粗排分与逐维解码后的 f32 点积一致(查询侧保持 f32)。
    #[test]
    fn query_score_matches_dequantized_dot() {
        let params = sample_params();
        let query = [0.5_f32, -1.25, 2.0, 0.0, 1.75];
        let prepared = Query::new(&query, &params).expect("Query::new");
        for vector in sample_vectors() {
            let codes = encode_row(&vector, &params);
            let restored = decode_row(&codes, &params);
            let expected: f32 = query.iter().zip(&restored).map(|(q, x)| q * x).sum();
            let got = prepared.score(&codes);
            assert!(
                (got - expected).abs() <= expected.abs().max(1.0) * 1e-4,
                "粗排分 {got} 与解码点积 {expected} 偏差过大"
            );
        }
    }

    /// 非法参数表:长度不符 / `v_min > v_max` / 非有限值 → `Corrupted`。
    #[test]
    fn invalid_table_is_rejected() {
        assert!(matches!(
            I8Params::from_table(&[0.0, 1.0], 2),
            Err(MnemeError::Corrupted { .. })
        ));
        assert!(matches!(
            I8Params::from_table(&[1.0, -1.0], 1),
            Err(MnemeError::Corrupted { .. })
        ));
        assert!(matches!(
            I8Params::from_table(&[f32::NAN, 1.0], 1),
            Err(MnemeError::Corrupted { .. })
        ));
    }
}
