//! L0 距离度量。
//!
//! 三种度量 [`Metric::Cosine`] / [`Metric::Dot`] / [`Metric::Euclidean`] 统一归结为
//! **一次点积**:范数列存储的是**范数平方**,于是
//!
//! $$
//! \|\mathbf{a}-\mathbf{b}\|^2 = \|\mathbf{a}\|^2 + \|\mathbf{b}\|^2 - 2\,\mathbf{a}\cdot\mathbf{b}.
//! $$
//!
//! 方向由 [`Metric::better`] 统一——所有 TopK、归并与排序都必须经 `better()` 比较,
//! **绝不直接比较 `score` 的数值大小**。

use crate::core::simd;

/// 检索分数。`Dot` / `Cosine` 越大越相似;`Euclidean` 返回距离平方(越小越近)。
pub type Score = f32;

/// 余弦分母下限:低于此值视为零向量,返回 0 而非 NaN(见设计 02 §3.3)。
const COSINE_EPSILON: f32 = 1e-12;

/// 距离度量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Metric {
    /// 余弦相似度 `a·b / (‖a‖·‖b‖)`,越大越相似。
    Cosine,
    /// 点积 `a·b`,越大越相似。
    Dot,
    /// 欧氏距离平方 `‖a−b‖²`,越小越近。
    Euclidean,
}

impl Metric {
    /// 计算原始分数。
    ///
    /// # Arguments
    ///
    /// * `a`、`b` - 两个等长向量。
    /// * `a_norm`、`b_norm` - 两向量的**范数平方**(`‖a‖²`、`‖b‖²`)。仅
    ///   [`Metric::Dot`] 不需要,可传 `0.0`。
    ///
    /// # Returns
    ///
    /// `Dot` 返回 `a·b`;`Cosine` 返回 `a·b / sqrt(a_norm * b_norm)`,分母低于
    /// `1e-12` 时返回 `0`;`Euclidean` 返回 `a_norm + b_norm - 2 * a·b`。
    ///
    /// # Panics
    ///
    /// 当 `a` 与 `b` 长度不等时,在 debug 构建下 panic;release 构建下按较短者计算。
    ///
    /// # Examples
    ///
    /// ```
    /// use mneme::Metric;
    ///
    /// assert_eq!(Metric::Dot.score(&[1.0, 2.0], &[3.0, 4.0], 0.0, 0.0), 11.0);
    /// ```
    pub fn score(&self, a: &[f32], b: &[f32], a_norm: f32, b_norm: f32) -> Score {
        self.score_from_dot(simd::dot(a, b), a_norm, b_norm)
    }

    /// 由**已算好的点积**按本度量口径折算分数。
    ///
    /// L6 量化粗排复用同一折算口径(仅点积为近似值),保证粗排分与精排分同向可比:
    /// `Dot` 直通点积;`Cosine` 除以范数乘积(零向量返回 0);`Euclidean` 返回
    /// `‖a‖² + ‖b‖² − 2·a·b`。
    ///
    /// # Arguments
    ///
    /// * `dot` - 两向量的点积(量化路径下为近似值)。
    /// * `a_norm`、`b_norm` - 两向量的**范数平方**。
    ///
    /// # Returns
    ///
    /// 与 [`Metric::score`] 同口径的分数。
    pub(crate) fn score_from_dot(&self, dot: f32, a_norm: f32, b_norm: f32) -> Score {
        match self {
            Metric::Dot => dot,
            Metric::Cosine => cosine_from_dot(dot, a_norm, b_norm),
            Metric::Euclidean => a_norm + b_norm - 2.0 * dot,
        }
    }

    /// 归一比较方向:`true` 表示 `x` 比 `y` 更优。
    ///
    /// `Cosine` / `Dot` 分数越大越优;`Euclidean`(距离平方)越小越优。
    ///
    /// # Arguments
    ///
    /// * `x` - 待比较的分数。
    /// * `y` - 基准分数。
    ///
    /// # Returns
    ///
    /// `x` 更优时返回 `true`:Cosine / Dot 下即 `x > y`,Euclidean 下即 `x < y`;
    /// 相等时返回 `false`。
    ///
    /// # Examples
    ///
    /// ```
    /// use mneme::Metric;
    ///
    /// assert!(Metric::Cosine.better(0.9, 0.1));
    /// assert!(Metric::Euclidean.better(0.1, 0.9));
    /// ```
    pub fn better(&self, x: Score, y: Score) -> bool {
        match self {
            Metric::Cosine | Metric::Dot => x > y,
            Metric::Euclidean => x < y,
        }
    }

    /// 分数的**全序**比较:`Metric::better` 方向;`NaN` 恒排最后(两个符号一视同仁),
    /// 数值相等时用 `total_cmp` 区分 `±0`;完全相等返回 `Ordering::Equal`。
    ///
    /// 供 `TopK` 淘汰与两阶段精排共用,保证含 `NaN` 分数时「入选集合」与「最终排序」
    /// 一致(FC-CORE-POST-002/004)。
    ///
    /// # Arguments
    ///
    /// * `a`、`b` - 待比较的两个分数。
    ///
    /// # Returns
    ///
    /// `a` 在最优序中位于 `b` 之前返回 `Less`;两者完全相等返回 `Equal`。
    pub(crate) fn score_order(&self, a: Score, b: Score) -> std::cmp::Ordering {
        if a.is_nan() || b.is_nan() {
            return a.is_nan().cmp(&b.is_nan());
        }
        if self.better(a, b) {
            std::cmp::Ordering::Less
        } else if self.better(b, a) {
            std::cmp::Ordering::Greater
        } else {
            a.total_cmp(&b)
        }
    }

    /// 是否需要范数列:`Cosine` / `Euclidean` 为 `true`,仅 `Dot` 为 `false`。
    ///
    /// # Returns
    ///
    /// `Dot` 返回 `false`;`Cosine` / `Euclidean` 返回 `true`。
    ///
    /// # Examples
    ///
    /// ```
    /// use mneme::Metric;
    ///
    /// assert!(!Metric::Dot.needs_norm());
    /// assert!(Metric::Cosine.needs_norm());
    /// ```
    pub const fn needs_norm(&self) -> bool {
        !matches!(self, Metric::Dot)
    }
}

/// 余弦相似度薄封装。
///
/// # Arguments
///
/// * `a_norm`、`b_norm` - 两向量的范数平方。
///
/// # Returns
///
/// `a·b / sqrt(a_norm * b_norm)`;当分母 `sqrt(a_norm * b_norm) = ‖a‖·‖b‖ < 1e-12`
/// (含零向量)时返回 `0`,绝不返回 `NaN`。
///
/// # Panics
///
/// 当 `a` 与 `b` 长度不等时,在 debug 构建下 panic。
///
/// # Examples
///
/// ```
/// use mneme::core::metric::cosine;
///
/// // ‖a‖² = 5,‖b‖² = 5,a·b = 4 → cos = 4 / 5 = 0.8
/// let s = cosine(&[1.0, 2.0], &[2.0, 1.0], 5.0, 5.0);
/// assert!((s - 0.8).abs() < 1e-6);
/// ```
pub fn cosine(a: &[f32], b: &[f32], a_norm: f32, b_norm: f32) -> Score {
    cosine_from_dot(simd::dot(a, b), a_norm, b_norm)
}

/// 由点积折算余弦相似度(量化粗排与精确路径共用)。
///
/// # Arguments
///
/// * `dot` - 两向量的点积(量化路径下为近似值)。
/// * `a_norm`、`b_norm` - 两向量的范数平方。
///
/// # Returns
///
/// `dot / (‖a‖·‖b‖)`;分母低于 `COSINE_EPSILON` 时返回 `0`。
pub(crate) fn cosine_from_dot(dot: f32, a_norm: f32, b_norm: f32) -> Score {
    let denominator = (a_norm * b_norm).sqrt();
    if denominator < COSINE_EPSILON {
        0.0
    } else {
        dot / denominator
    }
}

/// 欧氏距离平方薄封装。
///
/// # Arguments
///
/// * `a_norm`、`b_norm` - 两向量的范数平方。
///
/// # Returns
///
/// `a_norm + b_norm - 2 * a·b`。
///
/// # Panics
///
/// 当 `a` 与 `b` 长度不等时,在 debug 构建下 panic。
///
/// # Examples
///
/// ```
/// use mneme::core::metric::euclidean_sq;
///
/// // ‖a‖² = 5,‖b‖² = 52,a·b = 16 → ‖a−b‖² = 5 + 52 − 32 = 25
/// let d = euclidean_sq(&[1.0, 2.0], &[4.0, 6.0], 5.0, 52.0);
/// assert_eq!(d, 25.0);
/// ```
pub fn euclidean_sq(a: &[f32], b: &[f32], a_norm: f32, b_norm: f32) -> Score {
    a_norm + b_norm - 2.0 * simd::dot(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm_sq(v: &[f32]) -> f32 {
        v.iter().map(|x| x * x).sum()
    }

    #[test]
    fn dot_score_equals_inner_product() {
        let a = [1.0_f32, 2.0, 3.0];
        let b = [4.0_f32, 5.0, 6.0];
        assert_eq!(Metric::Dot.score(&a, &b, 0.0, 0.0), 32.0);
    }

    #[test]
    fn cosine_of_identical_vectors_is_one() {
        let a = [3.0_f32, 4.0];
        let s = Metric::Cosine.score(&a, &a, norm_sq(&a), norm_sq(&a));
        assert!((s - 1.0).abs() < 1e-6, "got {s}");
    }

    #[test]
    fn cosine_of_orthogonal_vectors_is_zero() {
        let a = [1.0_f32, 0.0];
        let b = [0.0_f32, 1.0];
        let s = Metric::Cosine.score(&a, &b, norm_sq(&a), norm_sq(&b));
        assert!(s.abs() < 1e-6, "got {s}");
    }

    #[test]
    fn euclidean_matches_direct_computation() {
        let a = [1.0_f32, 2.0];
        let b = [4.0_f32, 6.0];
        let expected = 3.0_f32 * 3.0 + 4.0 * 4.0;
        let s = Metric::Euclidean.score(&a, &b, norm_sq(&a), norm_sq(&b));
        assert!((s - expected).abs() < 1e-6, "got {s}");
    }

    #[test]
    fn better_direction_is_metric_aware() {
        assert!(Metric::Cosine.better(0.9, 0.1));
        assert!(!Metric::Cosine.better(0.1, 0.9));
        assert!(Metric::Dot.better(5.0, -5.0));
        assert!(Metric::Euclidean.better(0.1, 0.9));
        assert!(!Metric::Euclidean.better(0.9, 0.1));
    }

    #[test]
    fn needs_norm_only_dot_is_false() {
        assert!(!Metric::Dot.needs_norm());
        assert!(Metric::Cosine.needs_norm());
        assert!(Metric::Euclidean.needs_norm());
    }
}
