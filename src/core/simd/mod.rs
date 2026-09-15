//! L0 SIMD 点积内核与运行时分发。
//!
//! 本模块是全库两处 `unsafe` 白名单之一(另一处为 L2 `persist::source` 的
//! `MmapSource`),且每处均附 `// SAFETY:` 证明。架构相关的内联内核拆分到
//! 架构相关内联内核拆分到私有子模块 `x86` 与 `neon`,本文件只保留可移植入口与标量参考实现。
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

#[cfg(target_arch = "x86_64")]
pub(super) mod x86;

#[cfg(target_arch = "aarch64")]
pub(super) mod neon;

#[cfg(test)]
mod tests;

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
            // 计数在闭包内逐元素累加(与 `heap/ordering.rs::COMPARES` 同口径),
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
