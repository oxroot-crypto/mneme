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

    let mut dead = BitSet::default();
    for idx in 0..view.slots.len() {
        dead.set(idx);
    }
    let mut key_index = HashMap::new();
    let mut seqno = SeqNo::new(0);
    for (rowid, slot) in &latest {
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
        index: view.index.clone(),
        seqno,
        closed: view.closed,
    }
}
