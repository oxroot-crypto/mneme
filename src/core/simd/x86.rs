//! x86_64 内联内核(运行时分发见父模块 [`super`] 的模块文档)。
//!
//! 本文件是全库两处 `unsafe` 白名单之一(`src/core/simd/`);每个 `unsafe` 块
//! 均附 `// SAFETY:` 证明,`unsafe fn` 均说明调用者前置条件。

use std::arch::x86_64::{
    __m128, __m128i, _mm_add_ps, _mm_add_ss, _mm_cvtss_f32, _mm_loadl_epi64, _mm_loadu_ps,
    _mm_loadu_si128, _mm_movehl_ps, _mm_mul_ps, _mm_setzero_ps, _mm_shuffle_ps,
    _mm256_castps256_ps128, _mm256_cvtepi32_ps, _mm256_cvtepu8_epi32, _mm256_extractf128_ps,
    _mm256_fmadd_ps, _mm256_loadu_ps, _mm256_setzero_ps, _mm512_cvtepi32_ps, _mm512_cvtepu8_epi32,
    _mm512_fmadd_ps, _mm512_loadu_ps, _mm512_reduce_add_ps, _mm512_setzero_ps,
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
