//! WAL 帧应用(`recover/wal_replay.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
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

/// 推进 `RowId` 水位(仅当帧中 id 不低于当前水位)。
///
/// # Errors
/// 帧声称 `u64::MAX` 时已无法再分配下一号,返回 [`MnemeError::Corrupted`]
/// (FC-PERSIST-ERR-012),绝不 `+1` 溢出 panic 或回绕复用。
pub(super) fn advance_rowid(state: &mut WriterState, rowid: RowId) -> Result<()> {
    if rowid.get() >= state.next_rowid {
        state.next_rowid = rowid
            .get()
            .checked_add(1)
            .ok_or_else(|| MnemeError::Corrupted {
                segment: None,
                reason: "wal: rowid 水位已达 u64::MAX,无法推进".to_string(),
            })?;
    }
    Ok(())
}

/// 应用单帧到写状态。
fn apply_frame(state: &mut WriterState, seqno: u64, kind: FrameKind, payload: &[u8]) -> Result<()> {
    match kind {
        FrameKind::NsRegister => apply_ns_register(state, payload),
        FrameKind::NsUnregister => apply_ns_unregister(state, payload),
        FrameKind::RelKindRegister => apply_rel_kind_register(state, payload),
        FrameKind::Insert => apply_insert(state, payload),
        FrameKind::DeleteRow => {
            let (rowid, tx_ms) = wal::decode_delete_row(payload)?;
            let rowid = RowId::new(rowid);
            advance_rowid(state, rowid)?;
            state.tombstone(rowid, tx_ms, SeqNo::new(seqno))?;
            Ok(())
        }
        FrameKind::TouchRow => {
            let (rowid, at_ms, access_delta, _importance) = wal::decode_touch_row(payload)?;
            let stat = state.access.get_or_insert_default(RowId::new(rowid));
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
        state.next_ns_id = ns_id.checked_add(1).ok_or_else(|| MnemeError::Corrupted {
            segment: None,
            reason: "wal: ns_id 水位已达 u32::MAX,无法推进".to_string(),
        })?;
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

/// 应用 `RelKindRegister` 帧:登记自定义关系类型;冲突/编号非法 → `Corrupted`。
fn apply_rel_kind_register(state: &mut WriterState, payload: &[u8]) -> Result<()> {
    let (kind, name) = wal::decode_rel_kind_register(payload)?;
    state.register_recovered_rel_kind(kind, name)
}

/// 应用 `Insert` 帧。
fn apply_insert(state: &mut WriterState, payload: &[u8]) -> Result<()> {
    let (entry, vector, tx_ms) = wal::decode_insert(payload)?;
    let norm_sq = crate::memory::search::norm_sq(&vector);
    let vector = crate::memory::lazy::VectorStorage::owned(Arc::from(vector.into_boxed_slice()));
    let slot = slot_from_entry(
        state,
        SlotFromEntry {
            entry: &entry,
            vector,
            norm_sq,
            tx_ms,
            deleted: false,
        },
    );
    advance_rowid(state, entry.rowid)?;
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
    relation::upsert_edge_sharded(&mut state.out_edges, edge.clone());
    relation::upsert_edge_sharded(&mut state.in_edges, edge);
    Ok(())
}

/// 应用 `Unrelate` 帧。
fn apply_unrelate(state: &mut WriterState, payload: &[u8]) -> Result<()> {
    let (from, to, kind) = wal::decode_unrelate(payload)?;
    let (from, to) = (RowId::new(from), RowId::new(to));
    let kind = RelationKind(kind);
    relation::remove_edge_sharded(&mut state.out_edges, from, to, kind);
    relation::remove_edge_sharded(&mut state.in_edges, to, from, kind);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::metric::Metric;

    /// FC-PERSIST-ERR-012:合法 CRC 但 `rowid = u64::MAX` 的 `DeleteRow` 帧
    /// 无法推进水位 → `Corrupted`,绝不 `+1` 溢出 panic 或回绕复用 ID。
    #[test]
    fn wal_replay_rejects_max_rowid() {
        let mut bytes = wal::encode_file_header(1, Metric::Cosine).to_vec();
        bytes.extend_from_slice(&wal::encode_frame(
            1,
            FrameKind::DeleteRow,
            &wal::encode_delete_row(u64::MAX, 0),
        ));
        let mut state = WriterState::new();
        assert!(matches!(
            crate::persist::recover::replay_wal(&mut state, &bytes, 0, None),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    /// FC-PERSIST-ERR-012:合法 CRC 但 `ns_id = u32::MAX` 的 `NsRegister` 帧
    /// 无法推进水位 → `Corrupted`,绝不回绕复用 `NsId`。
    #[test]
    fn wal_replay_rejects_max_ns_id() {
        let mut bytes = wal::encode_file_header(1, Metric::Cosine).to_vec();
        bytes.extend_from_slice(&wal::encode_frame(
            1,
            FrameKind::NsRegister,
            &wal::encode_ns_register(u32::MAX, "n"),
        ));
        let mut state = WriterState::new();
        assert!(matches!(
            crate::persist::recover::replay_wal(&mut state, &bytes, 0, None),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    /// FC-PERSIST-ERR-012:`Insert` 帧同样经 checked 水位推进拒绝 `rowid = u64::MAX`。
    #[test]
    fn wal_replay_rejects_max_rowid_on_insert() {
        use crate::core::meta::Meta;
        use crate::core::types::{NsId, SeqNo};
        use crate::persist::msec::EntryData;
        let entry = EntryData {
            rowid: RowId::new(u64::MAX),
            seqno: SeqNo::new(1),
            ns_id: NsId::new(1),
            key: None,
            text: None,
            meta: Meta::Null,
            created_at_ms: 0,
            expires_at_ms: None,
            importance: None,
            access: None,
            valid_time: None,
            confidence: None,
            provenance: None,
        };
        let payload = wal::encode_insert(
            &entry,
            &[1.0, 0.0],
            0,
            crate::core::options::Compression::None,
        )
        .expect("encode_insert");
        let mut bytes = wal::encode_file_header(1, Metric::Cosine).to_vec();
        bytes.extend_from_slice(&wal::encode_frame(1, FrameKind::Insert, &payload));
        let mut state = WriterState::new();
        assert!(matches!(
            crate::persist::recover::replay_wal(&mut state, &bytes, 0, None),
            Err(MnemeError::Corrupted { .. })
        ));
    }
}
