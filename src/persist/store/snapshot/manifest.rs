//! 新段 MANIFEST 版本构造与快照发布(注册表排序、段条目水位)。

use std::sync::Arc;

use crate::core::error::Result;
use crate::memory::table::WriterState;
use crate::persist::flush;
use crate::persist::manifest::{Manifest, NsEntry, RelKindEntry, SegmentEntry};
use crate::persist::{FORMAT_VERSION, crc32};

use super::super::Store;

// reason: `next_manifest` 个 rustdoc 链接 `[`MnemeError::IdExhausted`]` 要名字在
// 作用域内才解析得到;代码本体只用 `Result`,故放行未用告警。
#[allow(unused_imports)]
use crate::core::error::MnemeError;

impl Store {
    /// 由当前写状态、刚物化的段(可缺省)与新增槽位构造下一个 MANIFEST 版本。
    ///
    /// # Errors
    /// 段号或 MANIFEST 版本水位耗尽时返回 [`MnemeError::IdExhausted`]
    /// (FC-PERSIST-ERR-012)。
    pub(super) fn next_manifest(&self, input: &NextManifestInput<'_>) -> Result<Manifest> {
        let mut segments = input.previous.segments.clone();
        let mut next_segment_id = input.previous.next_segment_id;
        for (chunk, encoded) in input.chunks.iter().zip(input.encoded) {
            segments.push(segment_entry(
                &SegmentEntryInput {
                    segment_id: next_segment_id,
                    ws: input.ws,
                    slot_indices: chunk,
                    now_ms: input.now_ms,
                },
                encoded,
            ));
            next_segment_id = crate::persist::manifest::next_segment_id(next_segment_id)?;
        }
        Ok(Manifest {
            dimension: self.dimension,
            metric: self.metric,
            stopwords: input.previous.stopwords,
            next_rel_kind: input.ws.next_rel_kind,
            manifest_version: crate::persist::manifest::next_manifest_version(
                input.previous.manifest_version,
            )?,
            watermark_seqno: input.ws.seqno.get(),
            next_rowid: input.ws.next_rowid,
            next_segment_id,
            next_ns_id: input.ws.next_ns_id,
            namespaces: manifest_namespaces(input.ws),
            rel_kinds: manifest_rel_kinds(input.ws),
            segments,
        })
    }

    /// 发布新 MANIFEST 快照并重置 WAL(Checkpoint)。
    pub(super) fn publish(&self, new_manifest: &Manifest) {
        self.publish_manifest(new_manifest);
        let mut wal = self
            .wal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // reason: Checkpoint 为空间回收;旧帧均 ≤ watermark,恢复时跳过。段与
        // MANIFEST 已提交,重置失败不阻断 flush(句柄安全由 `WalWriter::reset`
        // 内部重建/停用保证,FC-PERSIST-INV-005)。
        let _ = wal.reset().ok();
    }

    /// 只发布 MANIFEST 快照(不重置 WAL;compaction 用,未落盘尾部仍在 WAL 中)。
    pub(in crate::persist::store) fn publish_manifest(&self, new_manifest: &Manifest) {
        let mut guard = self
            .manifest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.version = new_manifest.manifest_version;
        guard.manifest = new_manifest.clone();
    }
}

/// [`Store::next_manifest`] 的输入(参数收敛)。
pub(super) struct NextManifestInput<'a> {
    /// 提交前的 MANIFEST。
    pub(super) previous: &'a Manifest,
    /// 写状态(水位/注册表)。
    pub(super) ws: &'a WriterState,
    /// 本批物化的新段(仅注册表变化时为空;与 `chunks` 一一对应)。
    pub(super) encoded: &'a [flush::EncodedSegment],
    /// 各新段包含的槽位(与 `encoded` 一一对应)。
    pub(super) chunks: &'a [&'a [usize]],
    /// 新段创建时刻(Unix 毫秒)。
    pub(super) now_ms: i64,
}

/// 单段 MANIFEST 条目的输入(参数收敛)。
struct SegmentEntryInput<'a> {
    /// 本段编号。
    segment_id: u32,
    /// 写状态(水位)。
    ws: &'a WriterState,
    /// 本段包含的全局槽位。
    slot_indices: &'a [usize],
    /// 段创建时刻(Unix 毫秒)。
    now_ms: i64,
}

/// 新段的 MANIFEST 条目。
fn segment_entry(input: &SegmentEntryInput<'_>, encoded: &flush::EncodedSegment) -> SegmentEntry {
    let (min_seqno, max_seqno) = seqno_range(input.ws, input.slot_indices);
    SegmentEntry {
        segment_id: input.segment_id,
        format_version: FORMAT_VERSION,
        row_count: input.slot_indices.len() as u64,
        min_seqno,
        max_seqno,
        created_ms: input.now_ms,
        vsec_crc: crc32(&encoded.vsec),
        msec_crc: crc32(&encoded.msec),
        hidx_crc: encoded.hidx.as_deref().map_or(0, crc32),
        entry_slot: encoded.entry_slot,
        entry_level: encoded.entry_level,
    }
}

/// 注册表条目(按 `NsId` 排序)。
fn manifest_namespaces(ws: &WriterState) -> Vec<NsEntry> {
    let mut namespaces: Vec<NsEntry> = ws
        .ns_registry
        .iter()
        .map(|(id, path)| NsEntry {
            ns_id: id.get(),
            path: Arc::clone(path),
        })
        .collect();
    namespaces.sort_by_key(|entry| entry.ns_id);
    namespaces
}

/// 由写状态的关系类型注册表构造 MANIFEST 条目(按编号升序,确定性编码)。
fn manifest_rel_kinds(ws: &WriterState) -> Vec<RelKindEntry> {
    let mut entries: Vec<RelKindEntry> = ws
        .rel_kind_names
        .iter()
        .map(|(&kind, name)| RelKindEntry {
            kind,
            name: Arc::clone(name),
        })
        .collect();
    entries.sort_by_key(|entry| entry.kind);
    entries
}

/// 计算指定槽位的 `(min_seqno, max_seqno)`;空集合返回 `(0, 0)`。
fn seqno_range(ws: &WriterState, slot_indices: &[usize]) -> (u64, u64) {
    let mut min = u64::MAX;
    let mut max = 0_u64;
    for &index in slot_indices {
        let value = ws.slots[index].seqno.get();
        min = min.min(value);
        max = max.max(value);
    }
    if slot_indices.is_empty() {
        (0, 0)
    } else {
        (min, max)
    }
}

/// 命名空间注册表相对 MANIFEST 是否有变化(注销/新增需要独立提交,即使无新槽位)。
pub(super) fn namespace_registry_changed(previous: &Manifest, ws: &WriterState) -> bool {
    if previous.namespaces.len() != ws.ns_registry.len() {
        return true;
    }
    previous.namespaces.iter().any(|entry| {
        ws.ns_registry
            .get(&crate::core::types::NsId::new(entry.ns_id))
            .is_none_or(|path| path.as_ref() != entry.path.as_ref())
    })
}
