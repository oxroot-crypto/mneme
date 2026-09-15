//! aarch64 NEON 内联内核(运行时分发见父模块 [`super`] 的模块文档)。
//!
//! 本文件属全库两处 `unsafe` 白名单(`src/core/simd/`);`unsafe` 块均附
//! `// SAFETY:` 证明,`unsafe fn` 均说明调用者前置条件。

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
