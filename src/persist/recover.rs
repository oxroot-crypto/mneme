//! 崩溃恢复:段文件 + WAL → 内存写状态(设计 04 §7)。
//!
//! 恢复把各活跃段的 `vsec`/`msec` 重建成版本链、key 索引与关系边,再回放
//! `seqno > watermark` 的 WAL 帧。墓碑以 `version_table.doc_offset =
//! TOMBSTONE_DOC_OFFSET` 表示(无记录体),重建为 `deleted = true` 的槽位,
//! 保证"删除永不复活"(I19)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::options::RelationKind;
use crate::core::types::{NsId, RowId, SeqNo};
use crate::memory::relation::{self, Edge};
use crate::memory::table::{AccessStat, SlotData, WriterState};
use crate::persist::manifest::Manifest;
use crate::persist::msec::{self, EntryData, VersionRow};
use crate::persist::vsec;
use crate::persist::wal::{self, FrameKind};

/// 一个待恢复的段:`(segment_id, vsec 字节, msec 字节)`。
pub(crate) struct SegmentBytes {
    /// 段编号。
    pub(crate) segment_id: u32,
    /// 向量段字节。
    pub(crate) vsec: Vec<u8>,
    /// 元数据段字节。
    pub(crate) msec: Vec<u8>,
}

/// 从零构建写状态,并载入命名空间注册表与 ID 水位。
pub(crate) fn empty_state(manifest: &Manifest) -> WriterState {
    let mut state = WriterState::new();
    for ns in &manifest.namespaces {
        let id = NsId::new(ns.ns_id);
        Arc::make_mut(&mut state.ns_registry).insert(id, Arc::clone(&ns.path));
        Arc::make_mut(&mut state.ns_by_path).insert(Arc::clone(&ns.path), id);
    }
    state.next_ns_id = manifest.next_ns_id;
    state.next_rowid = manifest.next_rowid;
    state.seqno = SeqNo::new(manifest.watermark_seqno);
    state
}

/// 把各段记录重建成写状态(调用前请先用 [`empty_state`] 载入注册表/水位)。
///
/// 返回被隔离(跳过)的段 id 列表,供调用方移入 `trash/`(设计 04 §7)。
///
/// # Errors
/// 段头/记录体损坏且 `fail_fast` 时返回错误;否则损坏段被跳过并计入返回值。
pub(crate) fn load_segments(
    state: &mut WriterState,
    segments: &[SegmentBytes],
    verify_payload: bool,
    fail_fast: bool,
) -> Result<Vec<u32>> {
    // 收集全部版本并按 (rowid, seqno) 全局排序,保证同一 RowId 的版本链有序。
    let mut versions: Vec<(VersionRow, usize)> = Vec::new();
    let mut parsed: Vec<(vsec::VsecView<'_>, msec::MsecView<'_>)> = Vec::new();
    let mut skipped: Vec<u32> = Vec::new();
    for segment in segments {
        let mut vsec_view = match vsec::parse(&segment.vsec) {
            Ok(view) => view,
            // 主版本过新一律拒绝打开(I18),绝不因 fail-fast 关闭而降级为跳过。
            Err(error) if fail_fast || matches!(error, MnemeError::UnsupportedVersion { .. }) => {
                return Err(error);
            }
            Err(_) => {
                skipped.push(segment.segment_id);
                continue;
            }
        };
        let mut msec_view = match msec::parse(&segment.msec) {
            Ok(view) => view,
            Err(error) if fail_fast || matches!(error, MnemeError::UnsupportedVersion { .. }) => {
                return Err(error);
            }
            Err(_) => {
                skipped.push(segment.segment_id);
                continue;
            }
        };
        if verify_payload {
            if let Err(error) = vsec_view.verify_payload() {
                if fail_fast {
                    return Err(error);
                }
                skipped.push(segment.segment_id);
                continue;
            }
            if let Err(error) = msec_view.verify_payload() {
                if fail_fast {
                    return Err(error);
                }
                skipped.push(segment.segment_id);
                continue;
            }
        }
        let index = parsed.len();
        for row in msec_view.version_rows()? {
            versions.push((row, index));
        }
        parsed.push((vsec_view, msec_view));
    }
    versions.sort_by_key(|(row, _)| (row.rowid, row.seqno));

    for (row, index) in &versions {
        let (vsec_view, msec_view) = &parsed[*index];
        let slot_id = row.slot_id as usize;
        let vector = vsec_view
            .vector(slot_id)
            .ok_or_else(|| MnemeError::Corrupted {
                segment: None,
                reason: "recover: vsec 缺少版本槽位向量".to_string(),
            })?;
        let body = msec_view.read_entry(row.doc_offset)?;
        let (slot_data, access) = match body {
            Some(entry) => {
                let access = entry.access;
                (
                    slot_from_entry(state, &entry, vector, row.tx_ms, false),
                    access,
                )
            }
            None => (
                tombstone_slot(
                    RowId::new(row.rowid),
                    SeqNo::new(row.seqno),
                    row.tx_ms,
                    vector,
                ),
                None,
            ),
        };
        let rowid = slot_data.rowid;
        let seqno = slot_data.seqno;
        // `commit_version` 会遮蔽同一 RowId 的上一版本(含墓碑),保证可见性正确。
        state.commit_version(rowid, slot_data)?;
        if seqno.get() > state.seqno.get() {
            state.seqno = seqno;
        }
        if row.rowid >= state.next_rowid {
            state.next_rowid = row.rowid + 1;
        }
        if let Some((last_access_ms, access_count)) = access {
            Arc::make_mut(&mut state.access).insert(
                rowid,
                AccessStat {
                    last_access_ms,
                    access_count,
                },
            );
        }
    }

    // 关系边(出边 + 入边)。
    for (_, msec_view) in &parsed {
        let edges = crate::persist::edges::parse(msec_view.relations_bytes())?;
        for edge in edges.forward {
            let built = Edge {
                from: RowId::new(edge.from),
                to: RowId::new(edge.to),
                kind: RelationKind(edge.kind),
                weight: edge.weight,
                metadata: edge.meta,
            };
            relation::upsert_edge(Arc::make_mut(&mut state.out_edges), built.clone());
            relation::upsert_edge(Arc::make_mut(&mut state.in_edges), built);
        }
    }
    state.pending.clear();
    Ok(skipped)
}

/// 由记录体 + 向量构造一个活槽位。
fn slot_from_entry(
    state: &WriterState,
    entry: &EntryData,
    vector: Vec<f32>,
    tx_ms: i64,
    deleted: bool,
) -> SlotData {
    let ns_path = state
        .ns_registry
        .get(&entry.ns_id)
        .cloned()
        .unwrap_or_else(|| Arc::from(""));
    let vector: Arc<[f32]> = Arc::from(vector.into_boxed_slice());
    let norm_sq = crate::memory::search::norm_sq(&vector);
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

/// 构造墓碑槽位(无记录体;仅保留可见性所需的标识与向量占位)。
fn tombstone_slot(rowid: RowId, seqno: SeqNo, tx_ms: i64, vector: Vec<f32>) -> SlotData {
    let vector: Arc<[f32]> = Arc::from(vector.into_boxed_slice());
    let norm_sq = crate::memory::search::norm_sq(&vector);
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

/// 回放 WAL:应用 `seqno > watermark` 的帧(设计 04 §3.3)。
///
/// # Errors
/// 文件头损坏或未知帧类型时返回错误(未 FLUSH 前的坏尾由 [`wal::replay`] 自行截断)。
pub(crate) fn replay_wal(state: &mut WriterState, bytes: &[u8], watermark: u64) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    // 批原子回放(I15):批内帧先暂存,遇 `BatchCommit` 才整体应用;
    // 文件在提交帧前结束则整批丢弃(设计 04 §3.3)。
    let mut batch: Option<Vec<(u64, FrameKind, Vec<u8>)>> = None;
    wal::visit_frames(bytes, |seqno, kind, payload| {
        match kind {
            FrameKind::BatchBegin => {
                batch = Some(Vec::new());
                return Ok(());
            }
            FrameKind::BatchCommit => {
                if let Some(buffered) = batch.take() {
                    for (inner_seqno, inner_kind, inner_payload) in buffered {
                        apply_if_after_watermark(
                            state,
                            inner_seqno,
                            inner_kind,
                            &inner_payload,
                            watermark,
                        )?;
                    }
                }
                return Ok(());
            }
            _ => {}
        }
        match batch.as_mut() {
            Some(buffered) => buffered.push((seqno, kind, payload.to_vec())),
            None => apply_if_after_watermark(state, seqno, kind, payload, watermark)?,
        }
        Ok(())
    })?;
    // 未闭合的批(缺 `BatchCommit`)整体丢弃。
    state.pending.clear();
    Ok(())
}

/// 仅应用 `seqno > watermark` 的帧,并推进内存水位。
fn apply_if_after_watermark(
    state: &mut WriterState,
    seqno: u64,
    kind: FrameKind,
    payload: &[u8],
    watermark: u64,
) -> Result<()> {
    // 注册帧是幂等元数据,且可能在首次 flush 前落盘(watermark 仍为 0),
    // 故不受水位约束,始终重放(设计 04 §3.3「注册与水位恢复」)。
    let metadata = matches!(kind, FrameKind::NsRegister | FrameKind::RelKindRegister);
    if !metadata && seqno <= watermark {
        return Ok(());
    }
    apply_frame(state, seqno, kind, payload)?;
    if seqno > state.seqno.get() {
        state.seqno = SeqNo::new(seqno);
    }
    Ok(())
}

/// 应用单帧到写状态。
fn apply_frame(state: &mut WriterState, seqno: u64, kind: FrameKind, payload: &[u8]) -> Result<()> {
    match kind {
        FrameKind::NsRegister => {
            let (ns_id, path) = wal::decode_ns_register(payload)?;
            let id = NsId::new(ns_id);
            Arc::make_mut(&mut state.ns_registry).insert(id, Arc::clone(&path));
            Arc::make_mut(&mut state.ns_by_path).insert(path, id);
            if ns_id >= state.next_ns_id {
                state.next_ns_id = ns_id + 1;
            }
        }
        FrameKind::Insert => {
            let (entry, vector) = wal::decode_insert(payload)?;
            let tx_ms = entry.created_at_ms;
            let slot = slot_from_entry(state, &entry, vector, tx_ms, false);
            if entry.rowid.get() >= state.next_rowid {
                state.next_rowid = entry.rowid.get() + 1;
            }
            state.commit_version(entry.rowid, slot)?;
        }
        FrameKind::DeleteRow => {
            let rowid = RowId::new(wal::decode_delete_row(payload)?);
            if rowid.get() >= state.next_rowid {
                state.next_rowid = rowid.get() + 1;
            }
            let _ = state.tombstone(rowid, 0, SeqNo::new(seqno))?;
        }
        FrameKind::TouchRow => {
            let (rowid, at_ms, access_delta, _importance) = wal::decode_touch_row(payload)?;
            let stat = Arc::make_mut(&mut state.access)
                .entry(RowId::new(rowid))
                .or_default();
            stat.access_count = stat.access_count.saturating_add(access_delta);
            stat.last_access_ms = at_ms;
        }
        FrameKind::Relate => {
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
        }
        FrameKind::Unrelate => {
            let (from, to, kind) = wal::decode_unrelate(payload)?;
            let (from, to) = (RowId::new(from), RowId::new(to));
            let kind = RelationKind(kind);
            relation::remove_edge(Arc::make_mut(&mut state.out_edges), from, to, kind);
            relation::remove_edge(Arc::make_mut(&mut state.in_edges), to, from, kind);
        }
        // 其余帧类型(Update/UpdateRow/BatchBegin/Commit 等)在对应路径处理或忽略。
        _ => {}
    }
    Ok(())
}
