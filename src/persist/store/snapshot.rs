//! 全量快照 flush 与备份(`store/snapshot.rs`)。
//!
//! `flush` 把整个写状态物化为一个新段、提交新 MANIFEST 并把旧段移入 `trash/`,
//! 随后重置 WAL(Checkpoint);`backup_to` 复制全部文件到目标目录,最后写
//! `current` 保证备份原子可用。

use std::path::Path;
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::memory::config::Config;
use crate::memory::table::WriterState;
use crate::persist::flush;
use crate::persist::manifest::{Manifest, NsEntry, SegmentEntry};
use crate::persist::storage::{
    self, CURRENT_FILE, SEGMENTS_DIR, WAL_DIR, manifest_name, msec_name, vsec_name,
};
use crate::persist::trash;
use crate::persist::{FORMAT_VERSION, crc32};

use super::Store;
use super::manifest_io;
use super::wal_writer::WAL_FILE;

/// 一次 flush 物化出的新段(段号、时间戳与两文件字节)。
struct BuiltSegment {
    id: u32,
    created_unix_ms: i64,
    vsec: Vec<u8>,
    msec: Vec<u8>,
}

impl Store {
    /// 全量快照 flush:写新段 + 提交 MANIFEST + 重置 WAL(设计 04 §3.2)。
    ///
    /// # Errors
    /// 只读模式返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(crate) fn flush(&self, ws: &WriterState, config: &Config) -> Result<()> {
        if self.read_only {
            return Err(MnemeError::Unsupported {
                feature: "只读模式写入",
            });
        }
        let created_unix_ms = config.clock.now_unix_ms();
        let (vsec, msec) = flush::build_segment(ws, config, created_unix_ms)?;
        let previous = self.manifest_snapshot();
        let segment = BuiltSegment {
            id: previous.next_segment_id,
            created_unix_ms,
            vsec,
            msec,
        };

        self.write_file(
            &format!("{SEGMENTS_DIR}/{}", vsec_name(segment.id)),
            &segment.vsec,
        )?;
        self.write_file(
            &format!("{SEGMENTS_DIR}/{}", msec_name(segment.id)),
            &segment.msec,
        )?;

        let new_manifest = self.next_manifest(&previous, ws, &segment);
        manifest_io::commit_manifest(&self.root, &new_manifest, self.hook.as_deref())?;

        // 旧段进入 trash 并清理;WAL 重置(所有覆盖条目已随快照物化)。
        trash::move_to_trash(&self.root, &old_segment_names(&previous))?;
        trash::purge(&self.root)?;
        self.publish(&new_manifest)?;
        Ok(())
    }

    /// 备份到目标目录(设计 16 §7)。
    ///
    /// 目标目录必须不存在或为空;先写全部段/MANIFEST/WAL,最后写 `current`,
    /// 中途失败不会留下可打开的备份。备份可被独立 `open`。
    ///
    /// # Errors
    /// 目标非空、目标等于源、或 I/O 失败时返回结构化错误。
    pub(crate) fn backup_to(&self, target: &Path) -> Result<crate::memory::ops::BackupReport> {
        if target == self.root {
            return Err(MnemeError::Config {
                reason: "备份目录不能是库目录本身",
            });
        }
        if storage::exists(target)? && !storage::list_dir(target, "")?.is_empty() {
            return Err(MnemeError::Busy("备份目标目录非空"));
        }
        storage::ensure_dir(target)?;
        storage::ensure_dir(&target.join(SEGMENTS_DIR))?;
        storage::ensure_dir(&target.join(WAL_DIR))?;

        let (version, manifest) = self.versioned_manifest();

        let mut counts = CopyCounts::default();
        for segment in &manifest.segments {
            // 被 MANIFEST 引用的段必须存在;缺失即备份不可信,绝不静默产出残档。
            copy_required(
                &self.root,
                target,
                &format!("{SEGMENTS_DIR}/{}", vsec_name(segment.segment_id)),
                &mut counts,
            )?;
            copy_required(
                &self.root,
                target,
                &format!("{SEGMENTS_DIR}/{}", msec_name(segment.segment_id)),
                &mut counts,
            )?;
        }
        copy_required(&self.root, target, &manifest_name(version), &mut counts)?;
        // WAL 可以在只读实例中不存在,故为可选。
        copy_optional(&self.root, target, WAL_FILE, &mut counts)?;
        let CopyCounts { files, bytes } = counts;
        // `current` 最后写:中途失败则备份不可打开,不会误认为完整。
        let current = version.to_string();
        storage::write_atomic(target, CURRENT_FILE, current.as_bytes())?;

        Ok(crate::memory::ops::BackupReport {
            files: files + 1,
            bytes: bytes + current.len() as u64,
            hardlinked: false,
        })
    }

    /// 由当前写状态与刚物化的段构造下一个 MANIFEST 版本。
    fn next_manifest(
        &self,
        previous: &Manifest,
        ws: &WriterState,
        segment: &BuiltSegment,
    ) -> Manifest {
        let (min_seqno, max_seqno) = seqno_range(ws);
        let mut namespaces: Vec<NsEntry> = ws
            .ns_registry
            .iter()
            .map(|(id, path)| NsEntry {
                ns_id: id.get(),
                path: Arc::clone(path),
            })
            .collect();
        namespaces.sort_by_key(|entry| entry.ns_id);

        Manifest {
            dimension: self.dimension,
            metric: self.metric,
            next_rel_kind: previous.next_rel_kind,
            manifest_version: previous.manifest_version + 1,
            watermark_seqno: ws.seqno.get(),
            next_rowid: ws.next_rowid,
            next_segment_id: segment.id + 1,
            next_ns_id: ws.next_ns_id,
            namespaces,
            rel_kinds: previous.rel_kinds.clone(),
            segments: vec![SegmentEntry {
                segment_id: segment.id,
                format_version: FORMAT_VERSION,
                row_count: ws.slots.len() as u64,
                min_seqno,
                max_seqno,
                created_ms: segment.created_unix_ms,
                vsec_crc: crc32(&segment.vsec),
                msec_crc: crc32(&segment.msec),
                hidx_crc: 0,
                entry_slot: 0,
                entry_level: 0,
            }],
        }
    }

    /// 发布新 MANIFEST 快照并重置 WAL(Checkpoint)。
    fn publish(&self, new_manifest: &Manifest) -> Result<()> {
        {
            let mut guard = self
                .manifest
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.version = new_manifest.manifest_version;
            guard.manifest = new_manifest.clone();
        }
        let mut wal = self
            .wal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        wal.reset()?;
        Ok(())
    }
}

/// 备份已复制文件数与字节数。
#[derive(Default)]
struct CopyCounts {
    files: usize,
    bytes: u64,
}

/// 复制一个必须存在的库内文件;缺失返回 [`MnemeError::Corrupted`]。
fn copy_required(root: &Path, target: &Path, rel: &str, counts: &mut CopyCounts) -> Result<()> {
    let content = storage::read_file(root, rel).map_err(|_| MnemeError::Corrupted {
        segment: None,
        reason: format!("备份:必存文件缺失或不可读:{rel}"),
    })?;
    counts.files += 1;
    counts.bytes += content.len() as u64;
    storage::write_atomic(target, rel, &content)
}

/// 复制一个可选文件(如只读实例中不存在的 WAL);缺失则跳过。
fn copy_optional(root: &Path, target: &Path, rel: &str, counts: &mut CopyCounts) -> Result<()> {
    if let Some(content) = storage::read_file_opt(root, rel)? {
        counts.files += 1;
        counts.bytes += content.len() as u64;
        storage::write_atomic(target, rel, &content)?;
    }
    Ok(())
}

/// 旧段文件名列表(用于移入 `trash/`)。
fn old_segment_names(previous: &Manifest) -> Vec<String> {
    previous
        .segments
        .iter()
        .flat_map(|segment| [vsec_name(segment.segment_id), msec_name(segment.segment_id)])
        .collect()
}

/// 计算槽位的 `(min_seqno, max_seqno)`;空表返回 `(0, 0)`。
fn seqno_range(ws: &WriterState) -> (u64, u64) {
    let mut min = u64::MAX;
    let mut max = 0_u64;
    for slot in ws.slots.iter() {
        let value = slot.seqno.get();
        min = min.min(value);
        max = max.max(value);
    }
    if ws.slots.is_empty() {
        (0, 0)
    } else {
        (min, max)
    }
}
