//! 字符串等值预筛 bloom(`analysis/bloom.rs`,设计 04 §5.3)。
//!
//! 双哈希(Kirsch–Mitzenmacher)布隆过滤器:`m` bit 位图、`k` 个位置。
//! 命中只会**多评估**几行,否定必定为真——因此位图饱和只降低剪枝率,
//! 绝不改变过滤语义。位图容量按构建期元素数预估,写路径持续增长后
//! 误报率上升但不漏报,属安全退化。

use crate::core::error::{MnemeError, Result};

/// FNV-1a 64 位偏移基。
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 位素数。
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// 第二哈希的种子(与第一哈希独立)。
const FNV_SECOND_SEED: u64 = 0x9e37_79b9_7f4a_7c15;
/// 最小位图位数(与 `u64` 字长对齐)。
const MIN_BITS: usize = 64;

/// 计算双哈希 `(h1, h2)`;`h2` 强制为奇数,保证第 i 个位置能覆盖位图。
fn hash_pair(value: &str) -> (u64, u64) {
    let mut h1 = FNV_OFFSET_BASIS;
    let mut h2 = FNV_SECOND_SEED;
    for byte in value.as_bytes() {
        h1 ^= u64::from(*byte);
        h1 = h1.wrapping_mul(FNV_PRIME);
        h2 ^= u64::from(*byte);
        h2 = h2.wrapping_mul(FNV_PRIME);
    }
    (h1, h2 | 1)
}

/// 固定容量布隆过滤器。
#[derive(Debug, Clone)]
pub(crate) struct BloomSet {
    bits: Vec<u64>,
    bit_len: usize,
    k: usize,
}

impl BloomSet {
    /// 按目标元素数与误判率构造。
    ///
    /// # Arguments
    /// * `capacity` - 预估元素数(写路径增长超限后饱和,不漏报)。
    /// * `fpp` - 目标误判率,须在 `(0,1)` 内(调用方已由 `Tuning` 校验)。
    pub(crate) fn new(capacity: usize, fpp: f32) -> Self {
        let k = optimal_k(fpp);
        let bits_per_item = k as f64 / std::f64::consts::LN_2;
        let raw = (capacity.max(1) as f64 * bits_per_item).ceil() as usize;
        let bit_len = raw.max(MIN_BITS).next_multiple_of(64);
        Self {
            bits: vec![0_u64; bit_len / 64],
            bit_len,
            k,
        }
    }

    /// 插入一个值。
    pub(crate) fn insert(&mut self, value: &str) {
        let (h1, h2) = hash_pair(value);
        for index in 0..self.k {
            let bit = position(h1, h2, index, self.bit_len);
            self.bits[bit / 64] |= 1_u64 << (bit % 64);
        }
    }

    /// 判定"可能存在";`false` 则必定不存在。
    pub(crate) fn maybe_contains(&self, value: &str) -> bool {
        let (h1, h2) = hash_pair(value);
        (0..self.k).all(|index| {
            let bit = position(h1, h2, index, self.bit_len);
            self.bits[bit / 64] & (1_u64 << (bit % 64)) != 0
        })
    }

    /// 位图位数(落盘编码用)。
    pub(crate) fn bit_len(&self) -> usize {
        self.bit_len
    }

    /// 哈希位置数(落盘编码用)。
    pub(crate) fn hash_count(&self) -> usize {
        self.k
    }

    /// 位图字(落盘编码用)。
    pub(crate) fn words(&self) -> &[u64] {
        &self.bits
    }

    /// 由落盘字段重建;结构非法(位数/字数/哈希数不符)返回 `Corrupted`。
    ///
    /// # Arguments
    /// * `bit_len` - 位图位数,须为非零的 64 倍数。
    /// * `k` - 哈希位置数,须在 `[1, 64]`。
    /// * `words` - 位图字,长度须等于 `bit_len / 64`。
    pub(crate) fn from_words(bit_len: usize, k: usize, words: Vec<u64>) -> Result<Self> {
        if bit_len < MIN_BITS || !bit_len.is_multiple_of(64) {
            return Err(corrupted("bloom: bit_len 非法"));
        }
        if !(1..=64).contains(&k) {
            return Err(corrupted("bloom: 哈希位置数越界"));
        }
        if words.len() != bit_len / 64 {
            return Err(corrupted("bloom: 位图长度与 bit_len 不符"));
        }
        Ok(Self {
            bits: words,
            bit_len,
            k,
        })
    }
}

/// 构造 bloom 结构损坏错误。
fn corrupted(reason: &'static str) -> MnemeError {
    MnemeError::Corrupted {
        segment: None,
        reason: reason.to_string(),
    }
}

/// 由目标误判率计算最优哈希位置数 `k = ⌈ln(1/p)/ln 2⌉`,夹在 `[1, 64]`。
///
/// 上限 64 与 [`BloomSet::from_words`] 的校验一致:极小 `fpp` 也必须落在
/// 可落盘、可重开读回的范围内,绝不生产自读不回的文件。
fn optimal_k(fpp: f32) -> usize {
    // 非有限/非正 `fpp` 视为最保守的极小值(`f32::clamp` 对 NaN 不生效,需显式兜底)。
    let p = if fpp.is_finite() && fpp > 0.0 {
        f64::from(fpp.min(1.0))
    } else {
        f64::MIN_POSITIVE
    };
    ((1.0_f64 / p).ln() / std::f64::consts::LN_2)
        .ceil()
        .clamp(1.0, 64.0) as usize
}

/// 第 `index` 个哈希位置(双哈希展开)。
fn position(h1: u64, h2: u64, index: usize, bit_len: usize) -> usize {
    let mixed = h1.wrapping_add((index as u64).wrapping_mul(h2));
    (mixed % bit_len as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserted_values_always_match() {
        let mut bloom = BloomSet::new(64, 0.01);
        for index in 0..64 {
            bloom.insert(&format!("key-{index}"));
        }
        for index in 0..64 {
            assert!(bloom.maybe_contains(&format!("key-{index}")));
        }
    }

    #[test]
    fn negative_lookup_is_filtered_for_small_set() {
        let mut bloom = BloomSet::new(64, 0.01);
        bloom.insert("alpha");
        assert!(!bloom.maybe_contains("definitely-absent-value"));
    }

    #[test]
    fn saturation_never_under_reports() {
        let mut bloom = BloomSet::new(1, 0.01);
        for index in 0..1_000 {
            bloom.insert(&format!("v{index}"));
        }
        for index in 0..1_000 {
            assert!(
                bloom.maybe_contains(&format!("v{index}")),
                "位图饱和不得产生漏报"
            );
        }
    }

    #[test]
    fn optimal_k_matches_formula() {
        assert_eq!(optimal_k(0.01), 7);
        assert_eq!(optimal_k(0.1), 4);
    }

    /// 极小 `fpp` 也必须夹在可落盘范围,自产文件必须能重开读回。
    #[test]
    fn extreme_fpp_stays_within_storable_k() {
        for fpp in [0.0_f32, 1e-30, f32::MIN_POSITIVE, 1.0, -1.0, f32::NAN] {
            let k = optimal_k(fpp);
            assert!((1..=64).contains(&k), "fpp={fpp}: k={k}");
            let bloom = BloomSet::new(64, fpp);
            assert!(
                BloomSet::from_words(bloom.bit_len(), bloom.hash_count(), bloom.words().to_vec())
                    .is_ok(),
                "fpp={fpp} 的 bloom 必须可重建"
            );
        }
    }
}
