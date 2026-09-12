//! 双时态历史视图(`temporal.rs`)。
//!
//! `as_of(t)` 在版本链上取「事务时间 ≤ t 的最新版本」组成一致快照(不变量 I26)。
//! `supersede` 的 `valid_to` 闭合在 `engine` 中完成,本模块只负责按事务时间重建视图。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::bitset::BitSet;
use crate::core::types::SeqNo;
use crate::memory::table::ReaderView;

/// 按事务时间上界 `tx_ms` 重建一份读视图。
///
/// 每个 `RowId` 取版本链中 `tx_ms ≤ t` 的最新版本;被墓碑遮蔽的版本不进入
/// `key_index`/`text_index`,但物理槽位仍保留以便 `iter_with(_, true)` 审计。
pub(crate) fn snapshot_at(view: &ReaderView, tx_ms: i64) -> ReaderView {
    let latest = historical_latest(view, tx_ms);
    let (dead, key_index, seqno) = history_visibility(view, &latest);
    ReaderView {
        slots: Arc::clone(&view.slots),
        dead: Arc::new(dead),
        key_index: Arc::new(key_index),
        versions: Arc::clone(&view.versions),
        latest: Arc::new(latest),
        out_edges: Arc::clone(&view.out_edges),
        in_edges: Arc::clone(&view.in_edges),
        access: Arc::clone(&view.access),
        ns_registry: Arc::clone(&view.ns_registry),
        indexes: Arc::clone(&view.indexes),
        slot_segment: Arc::clone(&view.slot_segment),
        reclaimed_versions: view.reclaimed_versions,
        inv: Arc::clone(&view.inv),
        zones: Arc::clone(&view.zones),
        key_bloom: Arc::clone(&view.key_bloom),
        seqno,
        closed: view.closed,
    }
}

/// 历史时点每个 RowId 可见的最新版本(`tx_ms` 之前最后一个版本)。
fn historical_latest(
    view: &ReaderView,
    tx_ms: i64,
) -> HashMap<crate::core::types::RowId, crate::core::types::SlotId> {
    let mut latest: HashMap<crate::core::types::RowId, crate::core::types::SlotId> = HashMap::new();
    for (rowid, chain) in view.versions.iter() {
        let mut chosen = None;
        for &slot in chain.iter() {
            let slot_data = &view.slots[slot.get() as usize];
            if slot_data.tx_ms <= tx_ms {
                chosen = Some(slot);
            } else {
                break;
            }
        }
        if let Some(slot) = chosen {
            latest.insert(*rowid, slot);
        }
    }
    latest
}

/// 历史视图的 `dead` 位图、`key` 索引与水位(墓碑不可见,活版本清除 `dead`)。
fn history_visibility(
    view: &ReaderView,
    latest: &HashMap<crate::core::types::RowId, crate::core::types::SlotId>,
) -> (
    BitSet,
    HashMap<(crate::core::types::NsId, crate::core::types::Key), crate::core::types::RowId>,
    SeqNo,
) {
    let mut dead = BitSet::default();
    for idx in 0..view.slots.len() {
        dead.set(idx);
    }
    let mut key_index = HashMap::new();
    let mut seqno = SeqNo::new(0);
    for (rowid, slot) in latest {
        let slot_data = &view.slots[slot.get() as usize];
        seqno = SeqNo::new(seqno.get().max(slot_data.seqno.get()));
        if slot_data.deleted {
            continue;
        }
        dead.clear(slot.get() as usize);
        if let Some(key) = &slot_data.key {
            key_index.insert((slot_data.ns_id, key.clone()), *rowid);
        }
    }
    (dead, key_index, seqno)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::metric::Metric;
    use crate::core::options::HnswParams;
    use crate::index::hnsw::HnswIndex;
    use crate::memory::table::WriterState;

    /// FC-INDEX-POST-005:`as_of` 重建的历史视图必须保留索引句柄,
    /// 否则会静默退化为全量暴力(性能回退不可见)。
    #[test]
    fn snapshot_at_preserves_index_handle() {
        let mut ws = WriterState::new();
        let built = Arc::new(HnswIndex::build(&[], HnswParams::default(), Metric::Dot));
        Arc::make_mut(&mut ws.indexes).push(crate::memory::index::SegmentIndex::new(
            0,
            built,
            Vec::new(),
        ));
        let view = ws.snapshot();
        assert!(!view.indexes.is_empty(), "构造的视图应携带索引");

        let snapshot = snapshot_at(&view, i64::MAX);
        assert!(
            !snapshot.indexes.is_empty(),
            "as_of 历史视图必须保留索引句柄(不得静默降级为暴力)"
        );
    }
}
