//! 版本行解码、并行调度同版本链提交。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::types::{RowId, SeqNo};
use crate::memory::lazy::{ByteSource, VectorStorage};
use crate::memory::table::{AccessStat, SlotData, WriterState};
use crate::persist::msec::VersionRow;
use crate::persist::vsec;

use super::super::state::ParsedSegment;
use super::slot::{SlotFromEntry, TombstoneSlotInput, slot_from_entry, tombstone_slot};

/// 并行解码阈值:版本数低于此值时直接顺序处理,避免线程开销。
const PARALLEL_DECODE_MIN: usize = 4096;

/// 按全局有序的版本链重建槽位。
///
/// 解码阶段(记录体解析 + 向量句柄构造)无副作用、可并行;提交阶段按全局序
/// 单线程执行(版本链 / key 索引 / 水位推进)。大批量时打开耗时的主体落在
/// 解码阶段,多核可线性摊薄(FC-PERSIST-INV-021 的冷启动优化)。
pub(in crate::persist::recover) fn apply_versions(
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
            tombstone_slot(TombstoneSlotInput {
                rowid: RowId::new(row.rowid),
                seqno: SeqNo::new(row.seqno),
                tx_ms: row.tx_ms,
                vector,
                norm_sq,
            }),
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
    super::super::wal_replay::advance_rowid(state, rowid)?;
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
