//! 段重建与写状态初始化(`recover/state.rs`)。
//!
//! 把各活跃段的 `vsec`/`msec` 重建成版本链、key 索引与关系边;墓碑以
//! `version_table.doc_offset = TOMBSTONE_DOC_OFFSET` 表示(无记录体),保证
//! "删除永不复活"(I19)。

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::types::{NsId, SeqNo};
use crate::memory::table::WriterState;
use crate::persist::manifest::Manifest;
use crate::persist::msec::{self, VersionRow};
use crate::persist::vsec;

use super::segment::{apply_relations, apply_versions, load_segment_views};

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
