//! 备份 `backup_to`:同盘优先硬链接,失败回退逐文件复制。

use std::path::Path;

use crate::core::error::{MnemeError, Result};
use crate::persist::manifest::Manifest;
use crate::persist::storage::{
    self, CURRENT_FILE, SEGMENTS_DIR, WAL_DIR, hidx_name, manifest_name, msec_name, vsec_name,
};

use super::super::Store;

/// 备份复制目标与统计(参数收敛);硬链接失败即把 `hardlinked` 置 `false`。
pub(super) struct CopySink<'a> {
    /// 源库根目录。
    pub(super) root: &'a Path,
    /// 备份目标目录。
    pub(super) target: &'a Path,
    /// 已复制文件数与字节数。
    pub(super) counts: CopyCounts,
    /// 是否全部走硬链接。
    pub(super) hardlinked: bool,
}

impl CopySink<'_> {
    /// 同盘优先硬链接一个必存段文件,失败时回退逐字节复制。
    pub(super) fn link_or_copy_required(&mut self, rel: &str) -> Result<()> {
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

impl Store {
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
        for rel in super::super::wal_writer::wal_files(self.storage.as_ref())? {
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
}

/// 备份已复制文件数与字节数。
#[derive(Default)]
pub(super) struct CopyCounts {
    pub(super) files: usize,
    pub(super) bytes: u64,
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
