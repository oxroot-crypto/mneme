use crate::core::simd;

/// 余弦相似度分母的零向量判定阈值。
const COSINE_EPSILON: f32 = 1e-12;

#[cfg(test)]
thread_local! {
    /// `cosine_from_norms` 调用次数(操作计数:验证 MMR/去重的对级计算上界)。
    pub(super) static COSINE_PAIRS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// 余弦相似度(零向量返回 0)。
pub(crate) fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    cosine_from_norms(simd::dot(a, b), simd::dot(a, a), simd::dot(b, b))
}

/// 由点积与两侧**自范数平方**求余弦:自范数可预计算复用(MMR/去重热路径),
/// 分母低于 `COSINE_EPSILON`(含零向量)时返回 0,与 [`cosine_sim`] 同口径。
pub(crate) fn cosine_from_norms(dot: f32, a_norm_sq: f32, b_norm_sq: f32) -> f32 {
    #[cfg(test)]
    COSINE_PAIRS.with(|count| count.set(count.get() + 1));
    let denom = (a_norm_sq * b_norm_sq).sqrt();
    if denom < COSINE_EPSILON {
        0.0
    } else {
        dot / denom
    }
}
