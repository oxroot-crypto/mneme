//! 检索加速结构:倒排索引 / zone map / bloom(设计 04 §5、06 §2–§3)。
//!
//! 三者随写路径增量维护,经 `Arc` 随 `ReaderView` 快照共享;查询期由 L4
//! 消费(块级下推、BM25 两遍统计)。墓碑与被遮蔽版本**不从索引删除**:
//! 统计时按视图可见性过滤,`as_of` 历史视图因此仍可检索旧版本文本。

mod bloom;
mod inv;
mod zones;

pub(crate) use bloom::BloomSet;
pub(crate) use inv::{InvertedIndex, Posting};
pub(crate) use zones::{MAX_EXACT_INT, ZONE_BLOCK_ROWS, ZoneIndex, ZoneKind};

/// bloom 位图的初始容量预估(元素数)。
///
/// 写路径持续增长超过该容量后只升误报率、不漏报;恢复/落盘重建时可
/// 按当前规模分配更大位图。
pub(crate) const BLOOM_INITIAL_CAPACITY: usize = 65_536;
