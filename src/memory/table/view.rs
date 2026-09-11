//! 不可变读视图(`table/view.rs`)。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::bitset::BitSet;
use crate::core::types::{Key, NsId, RowId, SeqNo, SlotId};
use crate::memory::index::VectorIndex;
use crate::memory::relation::Edge;

use super::AccessStat;
use super::state::SlotData;

/// 不可变读视图:读者克隆后无锁扫描。
pub(crate) struct ReaderView {
    pub(crate) slots: Arc<Vec<Arc<SlotData>>>,
    pub(crate) dead: Arc<BitSet>,
    pub(crate) key_index: Arc<HashMap<(NsId, Key), RowId>>,
    pub(crate) versions: Arc<HashMap<RowId, Vec<SlotId>>>,
    pub(crate) latest: Arc<HashMap<RowId, SlotId>>,
    pub(crate) out_edges: Arc<HashMap<RowId, Vec<Edge>>>,
    pub(crate) in_edges: Arc<HashMap<RowId, Vec<Edge>>>,
    pub(crate) access: Arc<HashMap<RowId, AccessStat>>,
    pub(crate) ns_registry: Arc<HashMap<NsId, Arc<str>>>,
    /// 覆盖槽位前缀的向量索引(与视图一同快照,保证快照一致)。
    pub(crate) index: Option<Arc<dyn VectorIndex>>,
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
