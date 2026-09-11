//! 增量段 flush 与备份(`store/snapshot.rs`)。
//!
//! `flush` 只把未落盘槽位与跨段 delta 物化为一个新段、提交新 MANIFEST 并
//! Checkpoint WAL(重置);已提交旧段保持活跃、write-once、不入 `trash/`。
//! `backup_to` 复制全部文件到目标目录,最后写 `current` 保证备份原子可用。

use std::path::Path;
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::memory::config::Config;
use crate::memory::table::WriterState;
use crate::persist::flush::{self, SegmentBuildInput};
use crate::persist::manifest::{Manifest, NsEntry, SegmentEntry};
use crate::persist::storage::{
    self, CURRENT_FILE, SEGMENTS_DIR, WAL_DIR, hidx_name, manifest_name, msec_name, vsec_name,
};
use crate::persist::{FORMAT_VERSION, crc32};

use super::Store;
use super::manifest_io;

/// 备份 MANIFEST 所列段:同盘优先硬链接,失败(跨盘/文件系统不支持)回退复制。
///
/// `hardlinked` 初值为「有待备份段」;任一文件回退复制即置 `false`(设计 16 §7.1)。
fn copy_manifest_segments(
    root: &Path,
    target: &Path,
    manifest: &Manifest,
    counts: &mut CopyCounts,
    hardlinked: &mut bool,
) -> Result<()> {
    for segment in &manifest.segments {
        // 被 MANIFEST 引用的段必须存在;缺失即备份不可信,绝不静默产出残档。
        link_or_copy_required(
            root,
            target,
            &format!("{SEGMENTS_DIR}/{}", vsec_name(segment.segment_id)),
            counts,
            hardlinked,
        )?;
        link_or_copy_required(
            root,
            target,
            &format!("{SEGMENTS_DIR}/{}", msec_name(segment.segment_id)),
            counts,
            hardlinked,
        )?;
        if segment.hidx_crc != 0 {
            link_or_copy_required(
                root,
                target,
                &format!("{SEGMENTS_DIR}/{}", hidx_name(segment.segment_id)),
                counts,
                hardlinked,
            )?;
        }
    }
    Ok(())
}

/// 同盘硬链接一个必存段文件;硬链接失败时回退逐字节复制(并清 `hardlinked`)。
fn link_or_copy_required(
    root: &Path,
    target: &Path,
    rel: &str,
    counts: &mut CopyCounts,
    hardlinked: &mut bool,
) -> Result<()> {
    let source = storage::resolve(root, rel)?;
    let destination = storage::resolve(target, rel)?;
    if std::fs::hard_link(&source, &destination).is_ok() {
        counts.files += 1;
        // reason: 统计为尽力而为;元数据读取失败仅少计字节,不影响备份正确性。
        counts.bytes += std::fs::metadata(&source).map_or(0, |metadata| metadata.len());
        return Ok(());
    }
    *hardlinked = false;
    copy_required(root, target, rel, counts)
}

impl Store {
    /// 增量段 flush:物化未落盘槽位 + delta + 提交 MANIFEST + Checkpoint WAL。
    ///
    /// 旧段保持活跃且 write-once;无新增槽位/ delta / 注册表变化时为空操作。
    /// HNSW 图随段写入 `hidx` 并安装到写状态(`ws.indexes`),未落盘尾部仍由
    /// 调用方暴力扫描。
    ///
    /// # Errors
    /// 只读模式返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(crate) fn flush(&self, ws: &mut WriterState, config: &Config) -> Result<()> {
        if self.read_only {
            return Err(MnemeError::Unsupported {
                feature: "只读模式写入",
            });
        }
        let previous = self.manifest_snapshot();
        let slot_indices = ws.unpersisted_slots();
        let full_relations = previous.segments.is_empty();
        let now_ms = config.clock.now_unix_ms();
        let delta = flush::build_delta(ws, &slot_indices, now_ms, full_relations);
        let registry_changed = namespace_registry_changed(&previous, ws);
        if slot_indices.is_empty() && delta.is_empty() && !registry_changed {
            return Ok(());
        }

        let encoded = if slot_indices.is_empty() && delta.is_empty() {
            None
        } else {
            Some(flush::build_segment(
                ws,
                config,
                now_ms,
                &SegmentBuildInput {
                    slots: &slot_indices,
                    delta: &delta,
                    full_relations,
                },
            )?)
        };
        let segment_id = previous.next_segment_id;

        if let Some(encoded) = &encoded {
            self.write_file(
                &format!("{SEGMENTS_DIR}/{}", vsec_name(segment_id)),
                &encoded.vsec,
            )?;
            self.write_file(
                &format!("{SEGMENTS_DIR}/{}", msec_name(segment_id)),
                &encoded.msec,
            )?;
            if let Some(hidx) = &encoded.hidx {
                self.write_file(&format!("{SEGMENTS_DIR}/{}", hidx_name(segment_id)), hidx)?;
            }
        }

        let new_manifest =
            self.next_manifest(&previous, ws, encoded.as_ref(), &slot_indices, now_ms);
        manifest_io::commit_manifest(&self.root, &new_manifest, self.hook.as_deref())?;

        // 段文件已原子提交:登记槽位归属与索引,清空 delta 标记;WAL 重置(Checkpoint)。
        if let Some(encoded) = encoded {
            ws.install_segment(segment_id, &slot_indices, encoded.index);
        }
        ws.clear_flush_dirty();
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
        let mut hardlinked = !manifest.segments.is_empty();
        copy_manifest_segments(&self.root, target, &manifest, &mut counts, &mut hardlinked)?;
        copy_required(&self.root, target, &manifest_name(version), &mut counts)?;
        // WAL 文件集可缺省(只读实例/刚 Checkpoint 后);逐文件复制。
        for rel in super::wal_writer::wal_files(&self.root)? {
            copy_optional(&self.root, target, &rel, &mut counts)?;
        }
        let CopyCounts { files, bytes } = counts;
        // `current` 最后写:中途失败则备份不可打开,不会误认为完整。
        let current = version.to_string();
        storage::write_atomic(target, CURRENT_FILE, current.as_bytes())?;

        Ok(crate::memory::ops::BackupReport {
            files: files + 1,
            bytes: bytes + current.len() as u64,
            hardlinked,
        })
    }

    /// 由当前写状态、刚物化的段(可缺省)与新增槽位构造下一个 MANIFEST 版本。
    fn next_manifest(
        &self,
        previous: &Manifest,
        ws: &WriterState,
        encoded: Option<&flush::EncodedSegment>,
        slot_indices: &[usize],
        now_ms: i64,
    ) -> Manifest {
        let mut namespaces: Vec<NsEntry> = ws
            .ns_registry
            .iter()
            .map(|(id, path)| NsEntry {
                ns_id: id.get(),
                path: Arc::clone(path),
            })
            .collect();
        namespaces.sort_by_key(|entry| entry.ns_id);

        let mut segments = previous.segments.clone();
        let next_segment_id = match encoded {
            Some(encoded) => {
                let (min_seqno, max_seqno) = seqno_range(ws, slot_indices);
                segments.push(SegmentEntry {
                    segment_id: previous.next_segment_id,
                    format_version: FORMAT_VERSION,
                    row_count: slot_indices.len() as u64,
                    min_seqno,
                    max_seqno,
                    created_ms: now_ms,
                    vsec_crc: crc32(&encoded.vsec),
                    msec_crc: crc32(&encoded.msec),
                    hidx_crc: encoded.hidx.as_deref().map_or(0, crc32),
                    entry_slot: encoded.entry_slot,
                    entry_level: encoded.entry_level,
                });
                previous.next_segment_id + 1
            }
            None => previous.next_segment_id,
        };

        Manifest {
            dimension: self.dimension,
            metric: self.metric,
            stopwords: previous.stopwords,
            next_rel_kind: previous.next_rel_kind,
            manifest_version: previous.manifest_version + 1,
            watermark_seqno: ws.seqno.get(),
            next_rowid: ws.next_rowid,
            next_segment_id,
            next_ns_id: ws.next_ns_id,
            namespaces,
            rel_kinds: previous.rel_kinds.clone(),
            segments,
        }
    }

    /// 发布新 MANIFEST 快照并重置 WAL(Checkpoint)。
    fn publish(&self, new_manifest: &Manifest) -> Result<()> {
        self.publish_manifest(new_manifest);
        let mut wal = self
            .wal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        wal.reset()?;
        Ok(())
    }

    /// 只发布 MANIFEST 快照(不重置 WAL;compaction 用,未落盘尾部仍在 WAL 中)。
    pub(super) fn publish_manifest(&self, new_manifest: &Manifest) {
        let mut guard = self
            .manifest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.version = new_manifest.manifest_version;
        guard.manifest = new_manifest.clone();
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
fn namespace_registry_changed(previous: &Manifest, ws: &WriterState) -> bool {
    if previous.namespaces.len() != ws.ns_registry.len() {
        return true;
    }
    previous.namespaces.iter().any(|entry| {
        ws.ns_registry
            .get(&crate::core::types::NsId::new(entry.ns_id))
            .is_none_or(|path| path.as_ref() != entry.path.as_ref())
    })
}
