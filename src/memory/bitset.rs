//! 可增长的位图(`bitset.rs`)。
//!
//! 用于按 `SlotId` 标记不可见物理版本;写状态与读视图经 `Arc` 共享该位图,
//! 写入经写时复制(COW)触发拷贝。

/// 位图每个字(`u64`)的位数。
const BITS_PER_WORD: usize = u64::BITS as usize;

/// 可增长的位图,用于标记不可见物理版本。
#[derive(Debug, Clone, Default)]
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

    /// 读取第 `idx` 位。
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
}
