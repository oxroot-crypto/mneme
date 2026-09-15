//! 写状态初始化同段回放个总编排。

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::types::{NsId, SeqNo};
use crate::memory::table::WriterState;
use crate::persist::manifest::Manifest;

use super::index::apply_indexes;
use super::parse::collect_versions;
use super::remap::{backfill_slot_segments, build_remaps};
use super::types::{RecoveredSegments, SegmentBytes};
use crate::persist::recover::segment::{apply_delta, apply_relations, apply_versions};

/// 从零构建写状态,并载入命名空间/关系类型注册表与 ID 水位。
///
/// # Errors
/// MANIFEST 的关系类型注册表冲突或编号非法时返回 [`MnemeError::Corrupted`]
/// (FC-MODEL-POST-008)。
pub(crate) fn empty_state(manifest: &Manifest) -> Result<WriterState> {
    let mut state = WriterState::new();
    for ns in &manifest.namespaces {
        let id = NsId::new(ns.ns_id);
        Arc::make_mut(&mut state.ns_registry).insert(id, Arc::clone(&ns.path));
        Arc::make_mut(&mut state.ns_by_path).insert(Arc::clone(&ns.path), id);
    }
    for rel in &manifest.rel_kinds {
        state.register_recovered_rel_kind(rel.kind, Arc::clone(&rel.name))?;
    }
    if manifest.next_rel_kind > state.next_rel_kind {
        state.next_rel_kind = manifest.next_rel_kind;
    }
    state.next_ns_id = manifest.next_ns_id;
    state.next_rowid = manifest.next_rowid;
    state.seqno = SeqNo::new(manifest.watermark_seqno);
    Ok(state)
}

/// 把各段记录重建成写状态(调用前请先用 [`empty_state`] 载入注册表/水位)。
///
/// 返回被隔离(跳过)的段 id 与各段重排映射,供调用方按需载入 hidx。
/// 段按编号升序处理(防御 MANIFEST 乱序):版本链全局排序,关系表与 delta
/// 逐段交错回放,保证时序正确。
///
/// # Errors
/// 段头/记录体损坏且 `fail_fast` 时返回错误;否则损坏段被跳过并计入返回值。
pub(crate) fn load_segments(
    state: &mut WriterState,
    segments: &[SegmentBytes],
    verify_payload: bool,
    fail_fast: bool,
) -> Result<RecoveredSegments> {
    // 恢复期间暂停索引增量维护:段载入完成后由磁盘索引或全量重建接管。
    state.is_indexing_paused = true;
    let collected = collect_versions(segments, verify_payload, fail_fast)?;
    let mut versions = collected.versions;
    let parsed = collected.parsed;
    let parsed_ids = collected.parsed_ids;
    let skipped = collected.skipped;
    versions.sort_by_key(|(row, _)| (row.rowid, row.seqno));
    // 记录被隔离(损坏)的段:compaction 计划必须排除它们,绝不把损坏段当活跃段
    // 合并清除(FC-PERSIST-ERR-006)。
    state.unavailable_segments = Arc::new(
        skipped
            .iter()
            .copied()
            .collect::<std::collections::HashSet<u32>>(),
    );

    // 每段构建"段内槽位 → 全局槽位"重排映射(倒排载入与 hidx 载入均需要)。
    let row_counts: Vec<usize> = parsed
        .iter()
        .map(|segment| segment.vsec_view.row_count() as usize)
        .collect();
    let remaps = build_remaps(&versions, &row_counts, &parsed_ids)?;

    apply_versions(state, &versions, &parsed)?;
    backfill_slot_segments(state, &remaps);
    for segment in parsed.iter() {
        apply_relations(state, &segment.msec_view)?;
        apply_delta(state, &segment.msec_view)?;
    }
    state.pending.clear();
    apply_indexes(state, &parsed, &remaps, fail_fast)?;
    state.is_indexing_paused = false;
    Ok(RecoveredSegments { skipped, remaps })
}
