//! WAL 帧应用(`recover/wal_replay.rs`)。

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::options::RelationKind;
use crate::core::types::{NsId, RowId, SeqNo};
use crate::memory::relation::{self, Edge};
use crate::memory::table::WriterState;
use crate::persist::wal::{self, FrameKind};

use super::segment::{SlotFromEntry, slot_from_entry};

/// 待应用的 WAL 帧视图(负载借用自回放缓冲)。
pub(super) struct PendingFrame<'a> {
    /// 写序号。
    pub(super) seqno: u64,
    /// 帧类型。
    pub(super) kind: FrameKind,
    /// 帧负载。
    pub(super) payload: &'a [u8],
}

/// 仅应用 `seqno > watermark` 的帧,并推进内存水位。
///
/// 所有帧(含注册/注销 metadata)都带真实 seqno 并统一受水位约束:已物化到
/// MANIFEST 的注册表是权威状态,残留旧 WAL 不得让已注销命名空间复活
/// (FC-PERSIST-POST-011、FC-LIFE-POST-006)。
pub(super) fn apply_if_after_watermark(
    state: &mut WriterState,
    frame: PendingFrame<'_>,
    watermark: u64,
) -> Result<()> {
    if frame.seqno <= watermark {
        return Ok(());
    }
    apply_frame(state, frame.seqno, frame.kind, frame.payload)?;
    if frame.seqno > state.seqno.get() {
        state.seqno = SeqNo::new(frame.seqno);
    }
    Ok(())
}

/// 应用单帧到写状态。
fn apply_frame(state: &mut WriterState, seqno: u64, kind: FrameKind, payload: &[u8]) -> Result<()> {
    match kind {
        FrameKind::NsRegister => apply_ns_register(state, payload),
        FrameKind::NsUnregister => apply_ns_unregister(state, payload),
        FrameKind::Insert => apply_insert(state, payload),
        FrameKind::DeleteRow => {
            let (rowid, tx_ms) = wal::decode_delete_row(payload)?;
            let rowid = RowId::new(rowid);
            if rowid.get() >= state.next_rowid {
                state.next_rowid = rowid.get() + 1;
            }
            state.tombstone(rowid, tx_ms, SeqNo::new(seqno))?;
            Ok(())
        }
        FrameKind::TouchRow => {
            let (rowid, at_ms, access_delta, _importance) = wal::decode_touch_row(payload)?;
            let stat = Arc::make_mut(&mut state.access)
                .entry(RowId::new(rowid))
                .or_default();
            stat.access_count = stat.access_count.saturating_add(access_delta);
            stat.last_access_ms = at_ms;
            Ok(())
        }
        FrameKind::Relate => apply_relate(state, payload),
        FrameKind::Unrelate => apply_unrelate(state, payload),
        // 其余帧类型(Update/UpdateRow/BatchBegin/Commit 等)在对应路径处理或忽略。
        _ => Ok(()),
    }
}

/// 应用 `NsRegister` 帧。
fn apply_ns_register(state: &mut WriterState, payload: &[u8]) -> Result<()> {
    let (ns_id, path) = wal::decode_ns_register(payload)?;
    let id = NsId::new(ns_id);
    Arc::make_mut(&mut state.ns_registry).insert(id, Arc::clone(&path));
    Arc::make_mut(&mut state.ns_by_path).insert(path, id);
    if ns_id >= state.next_ns_id {
        state.next_ns_id = ns_id + 1;
    }
    Ok(())
}

/// 应用 `NsUnregister` 帧:移除注册表项与路径映射;`NsId` 水位不回退、永不复用。
fn apply_ns_unregister(state: &mut WriterState, payload: &[u8]) -> Result<()> {
    let ns_id = NsId::new(wal::decode_ns_unregister(payload)?);
    if let Some(path) = Arc::make_mut(&mut state.ns_registry).remove(&ns_id) {
        Arc::make_mut(&mut state.ns_by_path).remove(&path);
    }
    Ok(())
}

/// 应用 `Insert` 帧。
fn apply_insert(state: &mut WriterState, payload: &[u8]) -> Result<()> {
    let (entry, vector, tx_ms) = wal::decode_insert(payload)?;
    let slot = slot_from_entry(
        state,
        SlotFromEntry {
            entry: &entry,
            vector,
            tx_ms,
            deleted: false,
        },
    );
    if entry.rowid.get() >= state.next_rowid {
        state.next_rowid = entry.rowid.get() + 1;
    }
    state.commit_version(entry.rowid, slot)?;
    Ok(())
}

/// 应用 `Relate` 帧。
fn apply_relate(state: &mut WriterState, payload: &[u8]) -> Result<()> {
    let (from, to, kind, weight, meta) = wal::decode_relate(payload)?;
    let edge = Edge {
        from: RowId::new(from),
        to: RowId::new(to),
        kind: RelationKind(kind),
        weight,
        metadata: meta,
    };
    relation::upsert_edge(Arc::make_mut(&mut state.out_edges), edge.clone());
    relation::upsert_edge(Arc::make_mut(&mut state.in_edges), edge);
    Ok(())
}

/// 应用 `Unrelate` 帧。
fn apply_unrelate(state: &mut WriterState, payload: &[u8]) -> Result<()> {
    let (from, to, kind) = wal::decode_unrelate(payload)?;
    let (from, to) = (RowId::new(from), RowId::new(to));
    let kind = RelationKind(kind);
    relation::remove_edge(Arc::make_mut(&mut state.out_edges), from, to, kind);
    relation::remove_edge(Arc::make_mut(&mut state.in_edges), to, from, kind);
    Ok(())
}
