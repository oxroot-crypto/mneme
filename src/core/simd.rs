//! L0 SIMD 点积内核与运行时分发。
//!
//! 本模块是全库**唯一允许出现 `unsafe`** 的位置,且每处均附 `// SAFETY:` 证明。
//! 对外只暴露 [`dot`] 与可移植参考实现 [`dot_scalar`]。
//!
//! # 运行时分发
//!
//! * x86_64:检测 `avx2` + `fma` → AVX2 内核;否则回退 SSE2(基线可用)。
//! * aarch64:使用 NEON 内核(NEON 为基线)。
//! * 其它架构:标量参考实现。

/// `_mm_shuffle_ps` 立即数:取每对 32 位元素中的高元素(即 `imm[1:0] = 0b01`)。
#[cfg(target_arch = "x86_64")]
const SHUFFLE_TAKE_HIGHEST: i32 = 0x1;

/// 计算两个等长 f32 向量的点积。
///
/// # Arguments
///
/// * `a`、`b` - 两个等长向量。
///
/// # Returns
///
/// `Σ aᵢ·bᵢ`。空切片返回 `0.0`。
///
/// # Panics
///
/// 当 `a` 与 `b` 长度不等时,在 debug 构建下 panic;release 构建下按较短者计算。
///
/// # Examples
///
/// ```
/// use mneme::simd::dot;
///
/// assert_eq!(dot(&[1.0, 2.0, 3.0], &[4.0, 5.0, 6.0]), 32.0);
/// ```
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "点积要求两向量等长");
    #[cfg(target_arch = "x86_64")]
    {
        dot_x86(a, b)
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: aarch64 保证 NEON 可用。
        unsafe { neon::dot_neon(a, b) }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        dot_scalar(a, b)
    }
}

/// 可移植标量参考实现。
///
/// 用于非 SIMD 架构回退、测试对照与可读性基准。
///
/// # Arguments
///
/// * `a`、`b` - 参与点积的两切片;长度不等时按较短者计算。
///
/// # Returns
///
/// `Σ aᵢ·bᵢ`;任一切片为空时返回 `0.0`。
///
/// # Examples
///
/// ```
/// use mneme::simd::dot_scalar;
///
/// assert_eq!(dot_scalar(&[1.0, 2.0], &[3.0, 4.0]), 11.0);
/// ```
pub fn dot_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(target_arch = "x86_64")]
fn dot_x86(a: &[f32], b: &[f32]) -> f32 {
    if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
        // SAFETY: 已确认 avx2 与 fma 均可用;长度前提由 dot 的 debug 断言与 n 收敛保证。
        unsafe { x86::dot_avx2(a, b) }
    } else {
        // SAFETY: x86_64 基线保证 SSE2 可用;长度前提同上。
        unsafe { x86::dot_sse2(a, b) }
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::{
        __m128, _mm_add_ps, _mm_add_ss, _mm_cvtss_f32, _mm_loadu_ps, _mm_movehl_ps, _mm_mul_ps,
        _mm_setzero_ps, _mm_shuffle_ps, _mm256_castps256_ps128, _mm256_extractf128_ps,
        _mm256_fmadd_ps, _mm256_loadu_ps, _mm256_setzero_ps,
    };

    /// AVX2 + FMA 点积内核。
    ///
    /// # Safety
    ///
    /// 调用者必须确认当前 CPU 支持 `avx2` 与 `fma`。
    #[target_feature(enable = "avx2", enable = "fma")]
    pub unsafe fn dot_avx2(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len().min(b.len());
        let mut i = 0usize;
        // SAFETY: 循环条件 i + 8 <= n 保证 [i, i+8) 落在两个切片范围内;CPU 支持已由调用者确认。
        let mut sum = unsafe {
            let mut acc = _mm256_setzero_ps();
            while i + 8 <= n {
                let va = _mm256_loadu_ps(a.as_ptr().add(i));
                let vb = _mm256_loadu_ps(b.as_ptr().add(i));
                acc = _mm256_fmadd_ps(va, vb, acc);
                i += 8;
            }
            let lo = _mm256_castps256_ps128(acc);
            let hi = _mm256_extractf128_ps(acc, 1);
            let sum128 = _mm_add_ps(lo, hi);
            let shuffled = _mm_movehl_ps(sum128, sum128);
            let pairs = _mm_add_ps(sum128, shuffled);
            let high = _mm_shuffle_ps(pairs, pairs, super::SHUFFLE_TAKE_HIGHEST);
            _mm_cvtss_f32(_mm_add_ss(pairs, high))
        };
        while i < n {
            sum += a[i] * b[i];
            i += 1;
        }
        sum
    }

    /// SSE2 点积内核(x86_64 基线,无需运行时检测)。
    ///
    /// # Safety
    ///
    /// 调用者必须确认当前 CPU 支持 `sse2`;x86_64 目标下恒成立。
    pub unsafe fn dot_sse2(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len().min(b.len());
        let mut i = 0usize;
        // SAFETY: 循环条件 i + 4 <= n 保证 [i, i+4) 落在两个切片范围内;x86_64 基线支持 SSE2。
        let mut sum = unsafe {
            let mut acc: __m128 = _mm_setzero_ps();
            while i + 4 <= n {
                let va = _mm_loadu_ps(a.as_ptr().add(i));
                let vb = _mm_loadu_ps(b.as_ptr().add(i));
                acc = _mm_add_ps(acc, _mm_mul_ps(va, vb));
                i += 4;
            }
            let shuffled = _mm_movehl_ps(acc, acc);
            let pairs = _mm_add_ps(acc, shuffled);
            let high = _mm_shuffle_ps(pairs, pairs, super::SHUFFLE_TAKE_HIGHEST);
            _mm_cvtss_f32(_mm_add_ss(pairs, high))
        };
        while i < n {
            sum += a[i] * b[i];
            i += 1;
        }
        sum
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::{vaddvq_f32, vdupq_n_f32, vfmaq_f32, vld1q_f32};

    /// NEON 点积内核。
    ///
    /// # Safety
    ///
    /// 调用者必须确认当前 CPU 支持 `neon`;aarch64 目标下恒成立。
    pub unsafe fn dot_neon(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len().min(b.len());
        let mut i = 0usize;
        // SAFETY: 循环条件 i + 4 <= n 保证 [i, i+4) 落在两个切片范围内;aarch64 基线支持 NEON。
        let mut sum = unsafe {
            let mut acc = vdupq_n_f32(0.0);
            while i + 4 <= n {
                let va = vld1q_f32(a.as_ptr().add(i));
                let vb = vld1q_f32(b.as_ptr().add(i));
                acc = vfmaq_f32(acc, va, vb);
                i += 4;
            }
            vaddvq_f32(acc)
        };
        while i < n {
            sum += a[i] * b[i];
            i += 1;
        }
        sum
    }
}

#[cfg(test)]
mod tests {
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
}
