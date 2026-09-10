//! 崩溃恢复:段文件 + WAL → 内存写状态(设计 04 §7)。
//!
//! 恢复把各活跃段的 `vsec`/`msec` 重建成版本链、key 索引与关系边,再回放
//! `seqno > watermark` 的 WAL 帧。墓碑以 `version_table.doc_offset =
//! TOMBSTONE_DOC_OFFSET` 表示(无记录体),重建为 `deleted = true` 的槽位,
//! 保证"删除永不复活"(I19)。
//!
//! # 子模块
//!
//! * `segment` —— 段视图解析与版本链/关系边重建。
//! * `wal_replay` —— WAL 帧应用。

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::types::{NsId, SeqNo};
use crate::memory::table::WriterState;
use crate::persist::manifest::Manifest;
use crate::persist::msec::{self, VersionRow};
use crate::persist::vsec;
use crate::persist::wal::{self, FrameKind};

mod segment;
mod wal_replay;

use self::segment::{apply_relations, apply_versions, load_segment_views};
use self::wal_replay::{PendingFrame, apply_if_after_watermark};

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
        let Some((vsec_view, msec_view)) = load_segment_views(segment, verify_payload, fail_fast)?
        else {
            skipped.push(segment.segment_id);
            continue;
        };
        let index = parsed.len();
        for row in msec_view.version_rows()? {
            versions.push((row, index));
        }
        parsed.push((vsec_view, msec_view));
    }
    versions.sort_by_key(|(row, _)| (row.rowid, row.seqno));

    apply_versions(state, &versions, &parsed)?;
    apply_relations(state, &parsed)?;
    state.pending.clear();
    Ok(skipped)
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
                            PendingFrame {
                                seqno: inner_seqno,
                                kind: inner_kind,
                                payload: &inner_payload,
                            },
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
            None => apply_if_after_watermark(
                state,
                PendingFrame {
                    seqno,
                    kind,
                    payload,
                },
                watermark,
            )?,
        }
        Ok(())
    })?;
    // 未闭合的批(缺 `BatchCommit`)整体丢弃。
    state.pending.clear();
    Ok(())
}
