//! L0 SIMD 点积内核与运行时分发。
//!
//! 本模块是全库两处 `unsafe` 白名单之一(另一处为 L2 `persist::source` 的
//! `MmapSource`),且每处均附 `// SAFETY:` 证明。
//! 对外只暴露 [`dot`] 与可移植参考实现 [`dot_scalar`]。
//!
//! # 运行时分发
//!
//! * x86_64:长度 ≥ `AVX512_MIN_LEN`(256)且检测到 `avx512f` → AVX-512F 内核
//!   (16 f32/次);否则检测 `avx2` + `fma` → AVX2 内核;再否则回退 SSE2(基线可用)。
//! * aarch64:使用 NEON 内核(NEON 为基线)。
//! * 其它架构:标量参考实现。
//!
//! 分发保留逐调用特性检测(检测缓存为廉价原子读);实测将其改为一次性缓存函数
//! 指针反而使 1536 维构建/查询回退(间接调用阻止内联),故不缓存。

/// `_mm_shuffle_ps` 立即数:取每对 32 位元素中的高元素(即 `imm[1:0] = 0b01`)。
#[cfg(target_arch = "x86_64")]
const SHUFFLE_TAKE_HIGHEST: i32 = 0x1;

/// 启用 AVX-512F 内核的最小向量长度。
///
/// 短向量下 512 位内核的收益不足以抵消宽指令的频率影响(实测 64 维基准变慢),
/// 故仅长向量(如 1536 维嵌入)走 AVX-512,短向量保持 AVX2。
#[cfg(target_arch = "x86_64")]
const AVX512_MIN_LEN: usize = 256;

#[cfg(test)]
thread_local! {
    /// 累计的 `dot_scalar` 逐元素乘加次数(操作计数,验证 $O(d)$;测试需自行清零)。
    static MUL_ADDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 取出当前累计的 `dot_scalar` 乘加计数(仅测试;计数跨调用累加,测试需自行清零)。
#[cfg(test)]
fn take_mul_adds() -> usize {
    MUL_ADDS.with(std::cell::Cell::get)
}

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
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| {
            // 计数在闭包内逐元素累加(与 `heap.rs::COMPARES` 同口径),
            // 而不是预先写入预期值——否则测试无法证伪"循环被改写/短路"。
            #[cfg(test)]
            MUL_ADDS.with(|count| count.set(count.get() + 1));
            x * y
        })
        .sum()
}

/// 计算 `u8` 码流与逐元素 f32 权重的点积(`Σ codesᵢ·weightsᵢ`)。
///
/// 供 L6 i8 量化粗排使用(设计 08 §2.3):码流每元素 1 字节,权重由查询向量与
/// 段级逐维参数预计算。长度不等时按较短者计算(与 [`dot`] 同口径)。
pub(crate) fn dot_u8_f32(codes: &[u8], weights: &[f32]) -> f32 {
    #[cfg(target_arch = "x86_64")]
    {
        if codes.len().min(weights.len()) >= AVX512_MIN_LEN && is_x86_feature_detected!("avx512f") {
            // SAFETY: 已确认 avx512f 可用;长度前提由内核循环边界保证。
            return unsafe { x86::dot_u8_avx512(codes, weights) };
        }
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            // SAFETY: 已确认 avx2 与 fma 均可用;长度前提由内核循环边界保证。
            return unsafe { x86::dot_u8_avx2(codes, weights) };
        }
        dot_u8_f32_scalar(codes, weights)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        dot_u8_f32_scalar(codes, weights)
    }
}

/// `u8 × f32` 点积的可移植标量参考实现(长度不等时按较短者)。
pub(crate) fn dot_u8_f32_scalar(codes: &[u8], weights: &[f32]) -> f32 {
    codes
        .iter()
        .zip(weights.iter())
        .map(|(&code, &weight)| f32::from(code) * weight)
        .sum()
}

#[cfg(target_arch = "x86_64")]
fn dot_x86(a: &[f32], b: &[f32]) -> f32 {
    if a.len().min(b.len()) >= AVX512_MIN_LEN && is_x86_feature_detected!("avx512f") {
        // SAFETY: 已确认 avx512f 可用;长度前提由内核循环边界保证。
        unsafe { x86::dot_avx512(a, b) }
    } else if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
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
        __m128, __m128i, _mm_add_ps, _mm_add_ss, _mm_cvtss_f32, _mm_loadl_epi64, _mm_loadu_ps,
        _mm_loadu_si128, _mm_movehl_ps, _mm_mul_ps, _mm_setzero_ps, _mm_shuffle_ps,
        _mm256_castps256_ps128, _mm256_cvtepi32_ps, _mm256_cvtepu8_epi32, _mm256_extractf128_ps,
        _mm256_fmadd_ps, _mm256_loadu_ps, _mm256_setzero_ps, _mm512_cvtepi32_ps,
        _mm512_cvtepu8_epi32, _mm512_fmadd_ps, _mm512_loadu_ps, _mm512_reduce_add_ps,
        _mm512_setzero_ps,
    };

    /// AVX-512F 点积内核:每次 16 个 f32 通道。
    ///
    /// # Safety
    ///
    /// 调用者必须确认当前 CPU 支持 `avx512f`。
    #[target_feature(enable = "avx512f")]
    pub unsafe fn dot_avx512(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len().min(b.len());
        let mut i = 0usize;
        // SAFETY: 循环条件 i + 16 <= n 保证 [i, i+16) 落在两个切片范围内;CPU 支持已由调用者确认。
        let mut sum = unsafe {
            let mut acc = _mm512_setzero_ps();
            while i + 16 <= n {
                let va = _mm512_loadu_ps(a.as_ptr().add(i));
                let vb = _mm512_loadu_ps(b.as_ptr().add(i));
                acc = _mm512_fmadd_ps(va, vb, acc);
                i += 16;
            }
            _mm512_reduce_add_ps(acc)
        };
        while i < n {
            sum += a[i] * b[i];
            i += 1;
        }
        sum
    }

    /// AVX-512F `u8 × f32` 点积内核:每次 16 个码位零扩展到 f32 后 FMA。
    ///
    /// # Safety
    ///
    /// 调用者必须确认当前 CPU 支持 `avx512f`。
    #[target_feature(enable = "avx512f")]
    pub unsafe fn dot_u8_avx512(codes: &[u8], weights: &[f32]) -> f32 {
        let n = codes.len().min(weights.len());
        let mut i = 0usize;
        // SAFETY: 循环条件 i + 16 <= n 保证 [i, i+16) 落在两个切片范围内;CPU 支持已由调用者确认。
        let mut sum = unsafe {
            let mut acc = _mm512_setzero_ps();
            while i + 16 <= n {
                let bytes = _mm_loadu_si128(codes.as_ptr().add(i).cast::<__m128i>());
                let widened = _mm512_cvtepu8_epi32(bytes);
                let floats = _mm512_cvtepi32_ps(widened);
                let weight = _mm512_loadu_ps(weights.as_ptr().add(i));
                acc = _mm512_fmadd_ps(floats, weight, acc);
                i += 16;
            }
            _mm512_reduce_add_ps(acc)
        };
        while i < n {
            sum += f32::from(codes[i]) * weights[i];
            i += 1;
        }
        sum
    }

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

    /// `u8 × f32` 点积内核(AVX2):每次 8 个码位零扩展到 f32 后 FMA,
    /// 读侧每元素 1 字节(设计 08 §2.3)。
    ///
    /// # Safety
    ///
    /// 调用者必须确认当前 CPU 支持 `avx2` 与 `fma`。
    #[target_feature(enable = "avx2", enable = "fma")]
    pub unsafe fn dot_u8_avx2(codes: &[u8], weights: &[f32]) -> f32 {
        let n = codes.len().min(weights.len());
        let mut i = 0usize;
        // SAFETY: 循环条件 i + 8 <= n 保证 [i, i+8) 落在两个切片范围内;CPU 支持已由调用者确认。
        let mut sum = unsafe {
            let mut acc = _mm256_setzero_ps();
            while i + 8 <= n {
                let bytes = _mm_loadl_epi64(codes.as_ptr().add(i).cast::<__m128i>());
                let widened = _mm256_cvtepu8_epi32(bytes);
                let floats = _mm256_cvtepi32_ps(widened);
                let weight = _mm256_loadu_ps(weights.as_ptr().add(i));
                acc = _mm256_fmadd_ps(floats, weight, acc);
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
            sum += f32::from(codes[i]) * weights[i];
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
}
