//! 段视图解析与版本链/关系边重建(`recover/segment.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::options::RelationKind;
use crate::core::types::{NsId, RowId, SeqNo};
use crate::memory::lazy::{ByteSource, VectorStorage};
use crate::memory::relation::{self, Edge};
use crate::memory::table::{AccessStat, SlotData, WriterState};
use crate::persist::msec::{self, EntryData, VersionRow};
use crate::persist::vsec;

use super::SegmentBytes;
use super::state::ParsedSegment;

/// `slot_from_entry` 的输入(记录体 + 向量存储 + 事务时间 + 墓碑标志)。
pub(super) struct SlotFromEntry<'a> {
    /// 记录体。
    pub(super) entry: &'a EntryData,
    /// 对应向量(自有或段内惰性)。
    pub(super) vector: Arc<VectorStorage>,
    /// 向量范数平方(vsec 范数列回读;无范数列时由解码回退算出)。
    pub(super) norm_sq: f32,
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
    let mut vsec_view = match vsec::parse(segment.vsec_bytes()?) {
        Ok(view) => view,
        // 版本不一致一律拒绝打开(I18),绝不因 fail-fast 关闭而降级为跳过。
        Err(error) if fail_fast || is_version_rejection(&error) => return Err(error),
        Err(_) => return Ok(None),
    };
    // f16 段在未开 `quant-f16` 的构建上拒绝打开,绝不静默按 f32 服务
    // (FC-QUANT-ERR-002);该判断先于 fail-fast 降级,数据仍完整可读也须显式报错。
    crate::quant::ensure_format_supported(vsec_view.quant())?;
    let mut msec_view = match msec::parse(segment.msec_bytes()?) {
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

/// 是否为格式版本不匹配导致的拒绝(不可降级跳过,I18)。
fn is_version_rejection(error: &MnemeError) -> bool {
    matches!(error, MnemeError::UnsupportedVersion { .. })
}

/// 并行解码阈值:版本数低于此值时直接顺序处理,避免线程开销。
const PARALLEL_DECODE_MIN: usize = 4096;

/// 按全局有序的版本链重建槽位。
///
/// 解码阶段(记录体解析 + 向量句柄构造)无副作用、可并行;提交阶段按全局序
/// 单线程执行(版本链 / key 索引 / 水位推进)。大批量时打开耗时的主体落在
/// 解码阶段,多核可线性摊薄(FC-PERSIST-INV-021 的冷启动优化)。
pub(super) fn apply_versions(
    state: &mut WriterState,
    versions: &[(VersionRow, usize)],
    parsed: &[ParsedSegment<'_>],
) -> Result<()> {
    let decoded = decode_versions(state, versions, parsed)?;
    let total = decoded.len();
    Arc::make_mut(&mut state.slots).reserve(total);
    state.slot_segment.reserve(total);
    state.versions.reserve(total);
    state.latest.reserve(total);
    // 解码结果保持 `(rowid, seqno)` 全局序:同一 `rowid` 的版本连续出现,
    // 据此判断是否存在旧版本可遮蔽(跳过大部分 `hide_latest` 查找)。
    let mut previous_rowid: Option<RowId> = None;
    for (slot_data, access) in decoded {
        let rowid = slot_data.rowid;
        commit_recovered_slot(state, slot_data, access, previous_rowid == Some(rowid))?;
        previous_rowid = Some(rowid);
    }
    Ok(())
}

/// 单个版本行的解码产物(纯数据,可跨线程移动)。
type DecodedVersion = (SlotData, Option<(i64, u32)>);

/// 解码全部版本行;大批量走 scoped threads,结果保持输入次序。
fn decode_versions(
    state: &WriterState,
    versions: &[(VersionRow, usize)],
    parsed: &[ParsedSegment<'_>],
) -> Result<Vec<DecodedVersion>> {
    let workers = if cfg!(feature = "wasm") {
        1
    } else {
        std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .min(versions.len())
    };
    if versions.len() < PARALLEL_DECODE_MIN || workers == 1 {
        let mut out = Vec::with_capacity(versions.len());
        for (row, index) in versions {
            out.push(decode_version(state, &parsed[*index], row)?);
        }
        return Ok(out);
    }
    let chunk = versions.len().div_ceil(workers);
    let pieces: Vec<Result<Vec<DecodedVersion>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = versions
            .chunks(chunk)
            .map(|chunk_versions| {
                scope.spawn(move || {
                    let mut out = Vec::with_capacity(chunk_versions.len());
                    for (row, index) in chunk_versions {
                        out.push(decode_version(state, &parsed[*index], row)?);
                    }
                    Ok(out)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                // reason: 解码是纯函数,线程 panic 只可能来自实现 bug;显式转内部
                // 不一致错误而非二次 panic(绝不静默丢版本)。
                handle.join().unwrap_or(Err(MnemeError::Inconsistent {
                    reason: "恢复解码线程 panic",
                }))
            })
            .collect()
    });
    let mut out = Vec::with_capacity(versions.len());
    for piece in pieces {
        out.extend(piece?);
    }
    Ok(out)
}

/// 解码单个版本行(无副作用;可在任意线程执行)。
fn decode_version(
    state: &WriterState,
    segment: &ParsedSegment<'_>,
    row: &VersionRow,
) -> Result<DecodedVersion> {
    let slot_id = row.slot_id as usize;
    let (vector, norm_sq) =
        vector_storage(&segment.vsec_view, Arc::clone(&segment.vsec_file), slot_id)?;
    let (slot_data, access) = match segment.msec_view.read_entry(row.doc_offset)? {
        Some(entry) => {
            let access = entry.access;
            (
                slot_from_entry(
                    state,
                    SlotFromEntry {
                        entry: &entry,
                        vector,
                        norm_sq,
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
                norm_sq,
            ),
            None,
        ),
    };
    Ok((slot_data, access))
}

/// 构造版本向量的存储形态:优先惰性句柄(范数列回读,不解码向量);
/// 无范数列时回退逐行解码并计算范数(旧布局防御,语义一致)。
fn vector_storage(
    vsec_view: &vsec::VsecView<'_>,
    vsec_file: Arc<crate::persist::source::ByteFile>,
    slot_id: usize,
) -> Result<(Arc<VectorStorage>, f32)> {
    let dimension = vsec_view.dimension() as usize;
    let offset = vsec_view.vector_offset(slot_id);
    if let (Some(offset), Some(norm_sq)) = (offset, vsec_view.norm_sq(slot_id))
        && let Some(storage) = VectorStorage::lazy(
            Arc::clone(&vsec_file) as Arc<dyn ByteSource>,
            offset,
            dimension,
        )
    {
        return Ok((storage, norm_sq));
    }
    // 回退:逐行解码(无范数列或句柄区间异常),仍不得静默丢弃版本。
    let vector = vsec_view
        .vector(slot_id)
        .ok_or_else(|| MnemeError::Corrupted {
            segment: None,
            reason: "recover: vsec 缺少版本槽位向量".to_string(),
        })?;
    let norm_sq = crate::memory::search::norm_sq(&vector);
    Ok((
        VectorStorage::owned(Arc::from(vector.into_boxed_slice())),
        norm_sq,
    ))
}

/// 提交恢复出的槽位并推进水位/访问统计。
fn commit_recovered_slot(
    state: &mut WriterState,
    slot_data: SlotData,
    access: Option<(i64, u32)>,
    previous_exists: bool,
) -> Result<()> {
    let rowid = slot_data.rowid;
    let seqno = slot_data.seqno;
    // 恢复专用提交:只建版本链(`commit_version` 会遮蔽同一 RowId 的上一版本)。
    state.commit_recovered(rowid, slot_data, previous_exists)?;
    if seqno.get() > state.seqno.get() {
        state.seqno = seqno;
    }
    super::wal_replay::advance_rowid(state, rowid)?;
    // 版本行的 `access` 列是写入时刻的累计快照:**同一 `rowid` 的后续版本必须覆盖**
    // (后一个版本的零快照要让更旧版本的值失效,跳过插入会让陈旧值残留并在 delta 上
    // 重复累加,FC-PERSIST-POST-010)。首个版本(无旧值可覆盖)为全零时可跳过——零值
    // 与缺失等价(`unwrap_or_default`),省去打开期每行一次分片插入。
    if let Some((last_access_ms, access_count)) = access
        && (previous_exists || last_access_ms != 0 || access_count != 0)
    {
        state.access.insert(
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
/// 带 `FLAG_FULL` 的段(首段/compaction 段)先重置关系表再应用;增量段仅 upsert,
/// 其后由 delta 区施加关系变更——增量段的 relations 区是空表,按全量重置会清掉
/// 先前段的边。全量语义保证 compaction 后已删除的边不会因并集而"复活"
/// (设计 04 §2.2b、FC-MODEL-POST-007)。
///
/// `RelationIndex::Both` 的段带反向表:入边由反向表 + 正向表并集恢复
/// (并集对损坏文件更稳健,upsert 幂等)。
pub(super) fn apply_relations(
    state: &mut WriterState,
    msec_view: &msec::MsecView<'_>,
) -> Result<()> {
    let edges = crate::persist::edges::parse(msec_view.relations_bytes())?;
    if edges.full {
        state.out_edges = crate::core::sharded::ShardedMap::new();
        state.in_edges = crate::core::sharded::ShardedMap::new();
    }
    let build = |edge: &crate::persist::edges::EdgeData| Edge {
        from: RowId::new(edge.from),
        to: RowId::new(edge.to),
        kind: RelationKind(edge.kind),
        weight: edge.weight,
        metadata: edge.meta.clone(),
    };
    for edge in &edges.forward {
        relation::upsert_edge_sharded(&mut state.out_edges, build(edge));
        relation::upsert_edge_sharded(&mut state.in_edges, build(edge));
    }
    for edge in &edges.reverse {
        relation::upsert_edge_sharded(&mut state.in_edges, build(edge));
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
        apply_delta_entry(state, entry);
    }
    Ok(())
}

/// 应用单条 delta 条目。
fn apply_delta_entry(state: &mut WriterState, entry: msec::DeltaEntry) {
    match entry {
        msec::DeltaEntry::Access {
            seqno,
            rowid,
            last_access_ms,
            access_delta,
            ..
        } => {
            if superseded_by_latest_version(state, rowid, seqno) {
                return;
            }
            let stat = state.access.get_or_insert_default(RowId::new(rowid));
            stat.access_count = stat.access_count.saturating_add(access_delta);
            // 时钟回拨下回放序可能倒退;保留更晚的访问时刻。
            stat.last_access_ms = stat.last_access_ms.max(last_access_ms);
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
            relation::upsert_edge_sharded(&mut state.out_edges, edge.clone());
            relation::upsert_edge_sharded(&mut state.in_edges, edge);
        }
        msec::DeltaEntry::Unrelate { from, to, kind, .. } => {
            apply_unrelate_delta(state, from, to, kind);
        }
    }
}

/// 应用 `Unrelate` delta:移除出边与入边。
fn apply_unrelate_delta(state: &mut WriterState, from: u64, to: u64, kind: u16) {
    relation::remove_edge_sharded(
        &mut state.out_edges,
        RowId::new(from),
        RowId::new(to),
        RelationKind(kind),
    );
    relation::remove_edge_sharded(
        &mut state.in_edges,
        RowId::new(to),
        RowId::new(from),
        RelationKind(kind),
    );
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

/// 由记录体 + 向量存储构造一个活槽位。
pub(super) fn slot_from_entry(state: &WriterState, input: SlotFromEntry<'_>) -> SlotData {
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

/// 构造墓碑槽位(无记录体;仅保留可见性所需的标识与向量占位)。
fn tombstone_slot(
    rowid: RowId,
    seqno: SeqNo,
    tx_ms: i64,
    vector: Arc<VectorStorage>,
    norm_sq: f32,
) -> SlotData {
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
