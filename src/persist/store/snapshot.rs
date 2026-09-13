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

/// 备份复制目标与统计(参数收敛);硬链接失败即把 `hardlinked` 置 `false`。
struct CopySink<'a> {
    /// 源库根目录。
    root: &'a Path,
    /// 备份目标目录。
    target: &'a Path,
    /// 已复制文件数与字节数。
    counts: CopyCounts,
    /// 是否全部走硬链接。
    hardlinked: bool,
}

impl CopySink<'_> {
    /// 同盘优先硬链接一个必存段文件,失败时回退逐字节复制。
    fn link_or_copy_required(&mut self, rel: &str) -> Result<()> {
        let source = storage::resolve(self.root, rel)?;
        let destination = storage::resolve(self.target, rel)?;
        if std::fs::hard_link(&source, &destination).is_ok() {
            self.counts.files += 1;
            // reason: 统计为尽力而为;元数据读取失败仅少计字节,不影响备份正确性。
            self.counts.bytes += std::fs::metadata(&source).map_or(0, |metadata| metadata.len());
            return Ok(());
        }
        self.hardlinked = false;
        self.copy_required(rel)
    }

    /// 复制一个必须存在的库内文件;缺失返回 [`MnemeError::Corrupted`]。
    fn copy_required(&mut self, rel: &str) -> Result<()> {
        // 被 MANIFEST 引用的段必须存在;缺失即备份不可信,绝不静默产出残档。
        let content =
            storage::read_file(self.root, rel).map_err(|error| MnemeError::Corrupted {
                segment: None,
                reason: format!("备份:必存文件缺失或不可读:{rel}: {error}"),
            })?;
        self.counts.files += 1;
        self.counts.bytes += content.len() as u64;
        storage::write_atomic(self.target, rel, &content)
    }
}

/// 一次增量 flush 提交所需的输入(参数收敛,避免超长参数表)。
struct CommitFlushInput {
    /// 提交前的 MANIFEST。
    previous: Manifest,
    /// 本次物化的未落盘槽位。
    slot_indices: Vec<usize>,
    /// 跨段 delta 条目。
    delta: Vec<crate::persist::msec::DeltaEntry>,
    /// 是否全量重写关系表(首段)。
    full_relations: bool,
    /// 新段创建时刻(Unix 毫秒)。
    now_ms: i64,
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
        if slot_indices.is_empty() && delta.is_empty() && !namespace_registry_changed(&previous, ws)
        {
            return Ok(());
        }
        self.commit_flush(
            ws,
            config,
            CommitFlushInput {
                previous,
                slot_indices,
                delta,
                full_relations,
                now_ms,
            },
        )
    }

    /// 编码新段(若有)并提交 MANIFEST,随后安装索引、清脏并发布新快照。
    fn commit_flush(
        &self,
        ws: &mut WriterState,
        config: &Config,
        input: CommitFlushInput,
    ) -> Result<()> {
        let CommitFlushInput {
            previous,
            slot_indices,
            delta,
            full_relations,
            now_ms,
        } = input;
        let segment_id = previous.next_segment_id;
        // 有新数据/delta 时写新段;仅注册表变化时只提交 MANIFEST。
        let encoded = self.encode_flush_segment(
            ws,
            config,
            &FlushSegmentInput {
                segment_id,
                slot_indices: &slot_indices,
                delta: &delta,
                full_relations,
                now_ms,
            },
        )?;

        let new_manifest = self.next_manifest(&NextManifestInput {
            previous: &previous,
            ws,
            encoded: encoded.as_ref(),
            slot_indices: &slot_indices,
            now_ms,
        })?;
        manifest_io::commit_manifest(&self.root, &new_manifest, self.hook.as_deref())?;

        // 段文件已原子提交:登记槽位归属与索引,清空 delta 标记;WAL 重置(Checkpoint)。
        if let Some(encoded) = encoded {
            ws.install_segment(
                segment_id,
                &slot_indices,
                encoded.index,
                encoded.quant,
                encoded.recall_est,
            );
        }
        ws.clear_flush_dirty();
        self.publish(&new_manifest);
        Ok(())
    }

    /// 有新增槽位/delta 时编码并写出新段;否则 `None`(仅注册表变化)。
    fn encode_flush_segment(
        &self,
        ws: &WriterState,
        config: &Config,
        input: &FlushSegmentInput<'_>,
    ) -> Result<Option<flush::EncodedSegment>> {
        if input.slot_indices.is_empty() && input.delta.is_empty() {
            return Ok(None);
        }
        let encoded = flush::build_segment(
            ws,
            config,
            input.now_ms,
            &SegmentBuildInput {
                slots: input.slot_indices,
                delta: input.delta,
                full_relations: input.full_relations,
            },
        )?;
        self.write_segment_files(input.segment_id, &encoded)?;
        Ok(Some(encoded))
    }

    /// 写入一个新段的 vsec/msec(以及可选 hidx)文件。
    fn write_segment_files(&self, segment_id: u32, encoded: &flush::EncodedSegment) -> Result<()> {
        let names = [
            format!("{SEGMENTS_DIR}/{}", vsec_name(segment_id)),
            format!("{SEGMENTS_DIR}/{}", msec_name(segment_id)),
            format!("{SEGMENTS_DIR}/{}", hidx_name(segment_id)),
        ];
        self.write_file(&names[0], &encoded.vsec)?;
        self.write_file(&names[1], &encoded.msec)?;
        if let Some(hidx) = &encoded.hidx {
            self.write_file(&names[2], hidx)?;
        }
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

        let mut sink = CopySink {
            root: &self.root,
            target,
            counts: CopyCounts::default(),
            hardlinked: !manifest.segments.is_empty(),
        };
        copy_segment_files(&mut sink, &manifest)?;
        sink.copy_required(&manifest_name(version))?;
        // WAL 文件集可缺省(只读实例/刚 Checkpoint 后);逐文件复制。
        for rel in super::wal_writer::wal_files(&self.root)? {
            copy_optional(&self.root, target, &rel, &mut sink.counts)?;
        }
        let CopyCounts { files, bytes } = sink.counts;
        // `current` 最后写:中途失败则备份不可打开,不会误认为完整。
        let current = version.to_string();
        storage::write_atomic(target, CURRENT_FILE, current.as_bytes())?;

        Ok(crate::memory::ops::BackupReport {
            files: files + 1,
            bytes: bytes + current.len() as u64,
            hardlinked: sink.hardlinked,
        })
    }

    /// 由当前写状态、刚物化的段(可缺省)与新增槽位构造下一个 MANIFEST 版本。
    ///
    /// # Errors
    /// 段号或 MANIFEST 版本水位耗尽时返回 [`MnemeError::IdExhausted`]
    /// (FC-PERSIST-ERR-012)。
    fn next_manifest(&self, input: &NextManifestInput<'_>) -> Result<Manifest> {
        let mut segments = input.previous.segments.clone();
        let next_segment_id = match input.encoded {
            Some(encoded) => {
                segments.push(segment_entry(input, encoded));
                crate::persist::manifest::next_segment_id(input.previous.next_segment_id)?
            }
            None => input.previous.next_segment_id,
        };
        Ok(Manifest {
            dimension: self.dimension,
            metric: self.metric,
            stopwords: input.previous.stopwords,
            next_rel_kind: input.previous.next_rel_kind,
            manifest_version: crate::persist::manifest::next_manifest_version(
                input.previous.manifest_version,
            )?,
            watermark_seqno: input.ws.seqno.get(),
            next_rowid: input.ws.next_rowid,
            next_segment_id,
            next_ns_id: input.ws.next_ns_id,
            namespaces: manifest_namespaces(input.ws),
            rel_kinds: input.previous.rel_kinds.clone(),
            segments,
        })
    }

    /// 发布新 MANIFEST 快照并重置 WAL(Checkpoint)。
    fn publish(&self, new_manifest: &Manifest) {
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
    pub(super) fn publish_manifest(&self, new_manifest: &Manifest) {
        let mut guard = self
            .manifest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.version = new_manifest.manifest_version;
        guard.manifest = new_manifest.clone();
    }
}

/// [`Store::encode_flush_segment`] 的输入(参数收敛)。
struct FlushSegmentInput<'a> {
    /// 新段编号。
    segment_id: u32,
    /// 本次物化的未落盘槽位。
    slot_indices: &'a [usize],
    /// 跨段 delta 条目。
    delta: &'a [crate::persist::msec::DeltaEntry],
    /// 是否全量重写关系表(首段)。
    full_relations: bool,
    /// 新段创建时刻(Unix 毫秒)。
    now_ms: i64,
}

/// [`Store::next_manifest`] 的输入(参数收敛)。
struct NextManifestInput<'a> {
    /// 提交前的 MANIFEST。
    previous: &'a Manifest,
    /// 写状态(水位/注册表)。
    ws: &'a WriterState,
    /// 本批物化的新段(仅注册表变化时为 `None`)。
    encoded: Option<&'a flush::EncodedSegment>,
    /// 新段包含的槽位。
    slot_indices: &'a [usize],
    /// 新段创建时刻(Unix 毫秒)。
    now_ms: i64,
}

/// 新段的 MANIFEST 条目。
fn segment_entry(input: &NextManifestInput<'_>, encoded: &flush::EncodedSegment) -> SegmentEntry {
    let (min_seqno, max_seqno) = seqno_range(input.ws, input.slot_indices);
    SegmentEntry {
        segment_id: input.previous.next_segment_id,
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

/// 备份已复制文件数与字节数。
#[derive(Default)]
struct CopyCounts {
    files: usize,
    bytes: u64,
}

/// 备份 MANIFEST 所列段(vsec/msec/可选 hidx);必存文件缺失即失败。
fn copy_segment_files(sink: &mut CopySink<'_>, manifest: &Manifest) -> Result<()> {
    for segment in &manifest.segments {
        // 被 MANIFEST 引用的段必须存在;缺失即备份不可信,绝不静默产出残档。
        sink.link_or_copy_required(&format!("{SEGMENTS_DIR}/{}", vsec_name(segment.segment_id)))?;
        sink.link_or_copy_required(&format!("{SEGMENTS_DIR}/{}", msec_name(segment.segment_id)))?;
        if segment.hidx_crc != 0 {
            sink.link_or_copy_required(&format!(
                "{SEGMENTS_DIR}/{}",
                hidx_name(segment.segment_id)
            ))?;
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// FC-LIFE-POST-008:硬链接失败(目标已存在/跨盘)时回退逐文件复制,
    /// `hardlinked` 如实置 `false`,产物内容正确。
    #[test]
    fn hardlink_failure_falls_back_to_copy() {
        let root = tempfile::tempdir().expect("root");
        let target = tempfile::tempdir().expect("target");
        let rel = format!("{SEGMENTS_DIR}/seg_000001.vsec");
        let source = storage::resolve(root.path(), &rel).expect("source path");
        storage::ensure_dir(source.parent().expect("parent")).expect("segments dir");
        let target_dir = storage::resolve(target.path(), &rel).expect("target path");
        storage::ensure_dir(target_dir.parent().expect("parent")).expect("target dir");
        let content = b"fake segment bytes";
        std::fs::write(&source, content).expect("write source");
        // 目标已存在同名文件 → `hard_link` 失败 → 走复制回退。
        std::fs::write(&target_dir, b"stale").expect("write stale");

        let mut sink = CopySink {
            root: root.path(),
            target: target.path(),
            counts: CopyCounts::default(),
            hardlinked: true,
        };
        sink.link_or_copy_required(&rel).expect("copy fallback");
        assert!(!sink.hardlinked, "硬链接失败必须如实报告回退");
        assert_eq!(sink.counts.files, 1);
        assert_eq!(sink.counts.bytes, content.len() as u64);
        assert_eq!(std::fs::read(&target_dir).expect("read"), content);
    }
}
