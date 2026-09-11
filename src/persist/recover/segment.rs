//! 段视图解析与版本链/关系边重建(`recover/segment.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::options::RelationKind;
use crate::core::types::{NsId, RowId, SeqNo};
use crate::memory::relation::{self, Edge};
use crate::memory::table::{AccessStat, SlotData, WriterState};
use crate::persist::msec::{self, EntryData, VersionRow};
use crate::persist::vsec;

use super::SegmentBytes;

/// `slot_from_entry` 的输入(记录体 + 向量 + 事务时间 + 墓碑标志)。
pub(super) struct SlotFromEntry<'a> {
    /// 记录体。
    pub(super) entry: &'a EntryData,
    /// 对应向量。
    pub(super) vector: Vec<f32>,
    /// 事务时间(Unix 毫秒)。
    pub(super) tx_ms: i64,
    /// 是否为墓碑。
    pub(super) deleted: bool,
}

/// 解析单个段的 vsec/msec 视图并校验 payload;损坏且非 fail-fast 时返回 `None`。
pub(super) fn load_segment_views<'a>(
    segment: &'a SegmentBytes,
    verify_payload: bool,
    fail_fast: bool,
) -> Result<Option<(vsec::VsecView<'a>, msec::MsecView<'a>)>> {
    let mut vsec_view = match vsec::parse(&segment.vsec) {
        Ok(view) => view,
        // 主版本过新一律拒绝打开(I18),绝不因 fail-fast 关闭而降级为跳过。
        Err(error) if fail_fast || is_version_rejection(&error) => return Err(error),
        Err(_) => return Ok(None),
    };
    let mut msec_view = match msec::parse(&segment.msec) {
        Ok(view) => view,
        Err(error) if fail_fast || is_version_rejection(&error) => return Err(error),
        Err(_) => return Ok(None),
    };
    if verify_payload {
        if let Err(error) = vsec_view.verify_payload() {
            if fail_fast {
                return Err(error);
            }
            return Ok(None);
        }
        if let Err(error) = msec_view.verify_payload() {
            if fail_fast {
                return Err(error);
            }
            return Ok(None);
        }
    }
    Ok(Some((vsec_view, msec_view)))
}

/// 是否为"主版本过新"导致的拒绝(不可降级跳过,I18)。
fn is_version_rejection(error: &MnemeError) -> bool {
    matches!(error, MnemeError::UnsupportedVersion { .. })
}

/// 按全局有序的版本链重建槽位。
pub(super) fn apply_versions(
    state: &mut WriterState,
    versions: &[(VersionRow, usize)],
    parsed: &[(vsec::VsecView<'_>, msec::MsecView<'_>)],
) -> Result<()> {
    for (row, index) in versions {
        let (vsec_view, msec_view) = &parsed[*index];
        apply_version(state, vsec_view, msec_view, row)?;
    }
    Ok(())
}

/// 应用单个版本行到写状态。
fn apply_version(
    state: &mut WriterState,
    vsec_view: &vsec::VsecView<'_>,
    msec_view: &msec::MsecView<'_>,
    row: &VersionRow,
) -> Result<()> {
    let slot_id = row.slot_id as usize;
    let vector = vsec_view
        .vector(slot_id)
        .ok_or_else(|| MnemeError::Corrupted {
            segment: None,
            reason: "recover: vsec 缺少版本槽位向量".to_string(),
        })?;
    let (slot_data, access) = match msec_view.read_entry(row.doc_offset)? {
        Some(entry) => {
            let access = entry.access;
            (
                slot_from_entry(
                    state,
                    SlotFromEntry {
                        entry: &entry,
                        vector,
                        tx_ms: row.tx_ms,
                        deleted: false,
                    },
                ),
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
    commit_recovered_slot(state, slot_data, access)
}

/// 提交恢复出的槽位并推进水位/访问统计。
fn commit_recovered_slot(
    state: &mut WriterState,
    slot_data: SlotData,
    access: Option<(i64, u32)>,
) -> Result<()> {
    let rowid = slot_data.rowid;
    let seqno = slot_data.seqno;
    // `commit_version` 会遮蔽同一 RowId 的上一版本(含墓碑),保证可见性正确。
    state.commit_version(rowid, slot_data)?;
    if seqno.get() > state.seqno.get() {
        state.seqno = seqno;
    }
    if rowid.get() >= state.next_rowid {
        state.next_rowid = rowid.get() + 1;
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
    Ok(())
}

/// 从单个段的 relations 区重建关系边(出边 + 入边)。
///
/// `RelationIndex::Both` 的段带反向表:入边由反向表 + 正向表并集恢复
/// (并集对损坏文件更稳健,upsert 幂等;设计 04 §2.2b、FC-MODEL-POST-007)。
pub(super) fn apply_relations(
    state: &mut WriterState,
    msec_view: &msec::MsecView<'_>,
) -> Result<()> {
    let edges = crate::persist::edges::parse(msec_view.relations_bytes())?;
    let build = |edge: &crate::persist::edges::EdgeData| Edge {
        from: RowId::new(edge.from),
        to: RowId::new(edge.to),
        kind: RelationKind(edge.kind),
        weight: edge.weight,
        metadata: edge.meta.clone(),
    };
    for edge in &edges.forward {
        relation::upsert_edge(Arc::make_mut(&mut state.out_edges), build(edge));
        relation::upsert_edge(Arc::make_mut(&mut state.in_edges), build(edge));
    }
    for edge in &edges.reverse {
        relation::upsert_edge(Arc::make_mut(&mut state.in_edges), build(edge));
    }
    Ok(())
}

/// 从单个段的 delta 区回放跨段访问统计与关系变更(设计 04 §2.2a)。
///
/// 访问统计按"版本行累计快照"语义恢复:delta 的 `seqno` 早于该 RowId 最新版本行
/// 的 `seqno` 时,说明后续版本行的 `access` 列已包含该增量,必须跳过,否则重开后
/// 计数虚高(FC-PERSIST-POST-010)。关系边是操作语义(upsert/remove),不参与该判定。
pub(super) fn apply_delta(state: &mut WriterState, msec_view: &msec::MsecView<'_>) -> Result<()> {
    for entry in msec::decode_delta(msec_view.delta_bytes())? {
        match entry {
            msec::DeltaEntry::Access {
                seqno,
                rowid,
                last_access_ms,
                access_delta,
                ..
            } => {
                if superseded_by_latest_version(state, rowid, seqno) {
                    continue;
                }
                let stat = Arc::make_mut(&mut state.access)
                    .entry(RowId::new(rowid))
                    .or_default();
                stat.access_count = stat.access_count.saturating_add(access_delta);
                stat.last_access_ms = last_access_ms;
            }
            msec::DeltaEntry::Relate {
                from,
                to,
                kind,
                weight,
                meta,
                ..
            } => {
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
            msec::DeltaEntry::Unrelate { from, to, kind, .. } => {
                relation::remove_edge(
                    Arc::make_mut(&mut state.out_edges),
                    RowId::new(from),
                    RowId::new(to),
                    RelationKind(kind),
                );
                relation::remove_edge(
                    Arc::make_mut(&mut state.in_edges),
                    RowId::new(to),
                    RowId::new(from),
                    RelationKind(kind),
                );
            }
        }
    }
    Ok(())
}

/// `Access` delta 是否已被该 RowId 更晚的版本行覆盖。
///
/// 版本行的 `access` 列是写入时刻的累计快照;delta 的 `seqno` 早于最新版本行的
/// `seqno` 时,该增量已计入版本行,不可再累加(FC-PERSIST-POST-010)。
/// RowId 无版本行(例如版本被回收)时返回 `false`,delta 必须照常应用。
fn superseded_by_latest_version(state: &WriterState, rowid: u64, delta_seqno: u64) -> bool {
    state
        .latest
        .get(&RowId::new(rowid))
        .is_some_and(|slot| state.slots[slot.get() as usize].seqno.get() > delta_seqno)
}

/// 由记录体 + 向量构造一个活槽位。
pub(super) fn slot_from_entry(state: &WriterState, input: SlotFromEntry<'_>) -> SlotData {
    let SlotFromEntry {
        entry,
        vector,
        tx_ms,
        deleted,
    } = input;
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
