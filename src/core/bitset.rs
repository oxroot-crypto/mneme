//! 可增长的位图(`bitset.rs`)。
//!
//! L0 纯数据结构:无 I/O、无锁。用于按 `SlotId` 标记不可见物理版本(L1),
//! 以及 L3 索引层的 alive / 过滤候选位图;写状态与读视图经 `Arc` 共享该位图,
//! 写入经写时复制(COW)触发拷贝。

/// 位图每个字(`u64`)的位数。
const BITS_PER_WORD: usize = u64::BITS as usize;

/// 可增长的位图,用于标记集合成员(按 `usize` 下标)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    /// 置位第 `idx` 位,必要时扩容。
    pub(crate) fn set(&mut self, idx: usize) {
        let word = idx / BITS_PER_WORD;
        if word >= self.words.len() {
            self.words.resize(word + 1, 0);
        }
        self.words[word] |= 1_u64 << (idx % BITS_PER_WORD);
    }

    /// 读取第 `idx` 位(越界返回 `false`)。
    pub(crate) fn get(&self, idx: usize) -> bool {
        self.words
            .get(idx / BITS_PER_WORD)
            .is_some_and(|word| (word >> (idx % BITS_PER_WORD)) & 1 == 1)
    }

    /// 清除第 `idx` 位。
    pub(crate) fn clear(&mut self, idx: usize) {
        if let Some(word) = self.words.get_mut(idx / BITS_PER_WORD) {
            *word &= !(1_u64 << (idx % BITS_PER_WORD));
        }
    }

    /// 已置位的总比特数(`popcount`)。
    pub(crate) fn count_ones(&self) -> usize {
        self.words
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_clear_and_count_roundtrip() {
        let mut bits = BitSet::default();
        assert_eq!(bits.count_ones(), 0);
        bits.set(0);
        bits.set(63);
        bits.set(64);
        bits.set(200);
        assert!(bits.get(0) && bits.get(63) && bits.get(64) && bits.get(200));
        assert!(!bits.get(1) && !bits.get(1000));
        assert_eq!(bits.count_ones(), 4);
        bits.clear(63);
        assert!(!bits.get(63));
        assert_eq!(bits.count_ones(), 3);
    }
}
