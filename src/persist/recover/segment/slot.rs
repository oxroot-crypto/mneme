//! 活槽位同墓碑槽位构造。

use std::sync::Arc;

use crate::core::meta::Meta;
use crate::core::types::{NsId, RowId, SeqNo};
use crate::memory::lazy::VectorStorage;
use crate::memory::table::{SlotData, WriterState};
use crate::persist::msec::EntryData;

/// `slot_from_entry` 的输入(记录体 + 向量存储 + 事务时间 + 墓碑标志)。
pub(in crate::persist::recover) struct SlotFromEntry<'a> {
    /// 记录体。
    pub(in crate::persist::recover) entry: &'a EntryData,
    /// 对应向量(自有或段内惰性)。
    pub(in crate::persist::recover) vector: Arc<VectorStorage>,
    /// 向量范数平方(vsec 范数列回读;无范数列时由解码回退算出)。
    pub(in crate::persist::recover) norm_sq: f32,
    /// 事务时间(Unix 毫秒)。
    pub(in crate::persist::recover) tx_ms: i64,
    /// 是否为墓碑。
    pub(in crate::persist::recover) deleted: bool,
}

/// 由记录体 + 向量存储构造一个活槽位。
pub(in crate::persist::recover) fn slot_from_entry(
    state: &WriterState,
    input: SlotFromEntry<'_>,
) -> SlotData {
    let SlotFromEntry {
        entry,
        vector,
        norm_sq,
        tx_ms,
        deleted,
    } = input;
    let ns_path = state
        .ns_registry
        .get(&entry.ns_id)
        .cloned()
        .unwrap_or_else(|| Arc::from(""));
    let text: Option<Arc<str>> = entry.text.clone();
    let text_hash = text
        .as_ref()
        .map(|text| crate::memory::dedup::fnv1a64(text.as_bytes()));
    let (valid_from, valid_to) = entry
        .valid_time
        .map_or((tx_ms, None), |(from, to)| (from, to));
    SlotData {
        rowid: entry.rowid,
        ns_id: entry.ns_id,
        ns_path,
        seqno: entry.seqno,
        key: entry.key.clone(),
        vector,
        norm_sq,
        text,
        text_hash,
        meta: entry.meta.clone(),
        created_at: entry.created_at_ms,
        expires_at: entry.expires_at_ms,
        importance: entry.importance.unwrap_or(0.5),
        confidence: entry.confidence.unwrap_or(1.0),
        valid_from,
        valid_to,
        provenance: entry.provenance.clone(),
        tx_ms,
        deleted,
    }
}

/// [`tombstone_slot`] 的输入参数。
#[derive(Debug)]
pub(super) struct TombstoneSlotInput {
    /// 稳定行标识。
    pub(super) rowid: RowId,
    /// 全局提交序号。
    pub(super) seqno: SeqNo,
    /// 事务时间(Unix 毫秒)。
    pub(super) tx_ms: i64,
    /// 向量存储占位(墓碑不参与检索,仅保持槽位形状)。
    pub(super) vector: Arc<VectorStorage>,
    /// 向量范数平方(墓碑占位)。
    pub(super) norm_sq: f32,
}

/// 构造墓碑槽位(无记录体;仅保留可见性所需的标识与向量占位)。
pub(super) fn tombstone_slot(input: TombstoneSlotInput) -> SlotData {
    let TombstoneSlotInput {
        rowid,
        seqno,
        tx_ms,
        vector,
        norm_sq,
    } = input;
    SlotData {
        rowid,
        ns_id: NsId::new(0),
        ns_path: Arc::from(""),
        seqno,
        key: None,
        vector,
        norm_sq,
        text: None,
        text_hash: None,
        meta: Meta::Null,
        created_at: tx_ms,
        expires_at: None,
        importance: 0.5,
        confidence: 1.0,
        valid_from: tx_ms,
        valid_to: None,
        provenance: None,
        tx_ms,
        deleted: true,
    }
}
