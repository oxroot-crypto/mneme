//! 不可变读视图(`table/view.rs`)。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::bitset::BitSet;
use crate::core::chunked::ChunkedVec;
use crate::core::sharded::ShardedMap;
use crate::core::types::{Key, NsId, RowId, SeqNo, SlotId};
use crate::memory::analysis::{BloomSet, InvertedIndex, ZoneIndex};
use crate::memory::index::SegmentIndex;
use crate::memory::relation::Edge;

use super::AccessStat;
use super::state::SlotData;

/// 不可变读视图:读者克隆后无锁扫描。
pub(crate) struct ReaderView {
    pub(crate) slots: Arc<Vec<Arc<SlotData>>>,
    pub(crate) dead: Arc<BitSet>,
    pub(crate) key_index: ShardedMap<(NsId, Key), RowId>,
    pub(crate) versions: ShardedMap<RowId, Arc<Vec<SlotId>>>,
    pub(crate) latest: ShardedMap<RowId, SlotId>,
    pub(crate) out_edges: ShardedMap<RowId, Vec<Edge>>,
    pub(crate) in_edges: ShardedMap<RowId, Vec<Edge>>,
    pub(crate) access: ShardedMap<RowId, AccessStat>,
    pub(crate) ns_registry: Arc<HashMap<NsId, Arc<str>>>,
    /// 命名空间路径 → `NsId`(与 `ns_registry` 互逆;查询期 $O(1)$ 解析)。
    pub(crate) ns_by_path: Arc<HashMap<Arc<str>, NsId>>,
    /// 各已落盘段的向量索引(与视图一同快照,保证快照一致;空 = 恒暴力)。
    pub(crate) indexes: Arc<Vec<SegmentIndex>>,
    /// 与 `slots` 平行的"槽位 → 所属段编号"(`None` = 未落盘尾部)。
    pub(crate) slot_segment: ChunkedVec<Option<u32>>,
    /// 本进程累计物理回收的版本数(compaction)。
    pub(crate) reclaimed_versions: u64,
    /// 内存倒排索引(BM25 两遍统计;与视图一同快照)。
    pub(crate) inv: Arc<InvertedIndex>,
    /// 块级 zone map(过滤下推)。
    pub(crate) zones: Arc<ZoneIndex>,
    /// `key` 字段的布隆预筛。
    pub(crate) key_bloom: Arc<BloomSet>,
    pub(crate) seqno: SeqNo,
    /// 库是否已关闭(关闭后读写返回 `Closed`)。
    pub(crate) closed: bool,
}

impl ReaderView {
    /// 返回 `rowid` 当前可见的物理槽位(被遮蔽/墓碑则为 `None`)。
    pub(crate) fn live_slot(&self, rowid: RowId) -> Option<SlotId> {
        let slot = *self.latest.get(&rowid)?;
        if self.dead.get(slot.get() as usize) {
            return None;
        }
        let slot_data = self.slots.get(slot.get() as usize)?;
        if slot_data.deleted {
            return None;
        }
        Some(slot)
    }

    /// 按 `(ns_id, key)` 解析稳定 `RowId`。
    pub(crate) fn rowid_of_key(&self, ns_id: NsId, key: &Key) -> Option<RowId> {
        self.key_index.get(&(ns_id, key.clone())).copied()
    }
}
