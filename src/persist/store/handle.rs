//! L2 持久化协调句柄 `Store` 的定义与统计/校验辅助(`store/handle.rs`)。
//!
//! `Store` 持有目录、独占锁、WAL 写入器与当前 MANIFEST,实现内存引擎的
//! [`PersistHook`](crate::memory::table::PersistHook);具体的打开/落盘/备份
//! 实现分散在同目录的 `open`/`snapshot`/`hook` 等子模块。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::persist::hook::FsyncHook;
use crate::persist::manifest::Manifest;
use crate::persist::msec::{self, TOMBSTONE_DOC_OFFSET};
use crate::persist::storage::{self, FileLock, SEGMENTS_DIR, msec_name, vsec_name};
use crate::persist::trash;
use crate::persist::vsec;

use super::wal_writer::{WAL_FILE, WalWriter};

/// 当前 MANIFEST 快照(受 `Mutex` 保护)。
pub(crate) struct ManifestState {
    pub(super) version: u64,
    pub(super) manifest: Manifest,
}

/// L2 持久化协调句柄。
pub(crate) struct Store {
    pub(super) root: PathBuf,
    pub(super) lock: Mutex<Option<FileLock>>,
    pub(super) wal: Mutex<WalWriter>,
    pub(super) manifest: Mutex<ManifestState>,
    pub(super) dimension: u32,
    pub(super) metric: Metric,
    pub(super) read_only: bool,
    pub(super) hook: Option<Arc<dyn FsyncHook>>,
}

impl Store {
    /// 当前 WAL 文件字节数。
    ///
    /// WAL 不存在/元数据不可读时返回 0(统计为尽力而为,不阻断调用方)。
    pub(crate) fn wal_bytes(&self) -> u64 {
        storage::resolve(&self.root, WAL_FILE)
            .ok()
            .and_then(|path| std::fs::metadata(path).ok())
            .map_or(0, |metadata| metadata.len())
    }

    /// `trash/` 目录字节数(统计为尽力而为,不可读时返回 0)。
    pub(crate) fn trash_bytes(&self) -> u64 {
        // reason: stats 为尽力而为;trash 读取失败仅少计字节,不影响正确性。
        trash::bytes(&self.root).unwrap_or(0)
    }

    /// 活跃段统计(来自当前 MANIFEST)。
    pub(crate) fn segment_stats(&self) -> Vec<crate::memory::ops::SegmentStat> {
        let guard = self
            .manifest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .manifest
            .segments
            .iter()
            .map(|segment| {
                segment_stat(
                    &self.root,
                    segment.segment_id,
                    segment.row_count,
                    segment.created_ms,
                )
            })
            .collect()
    }

    /// 活跃段数。
    pub(crate) fn total_segments(&self) -> usize {
        self.manifest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .manifest
            .segments
            .len()
    }

    /// 校验全部活跃段:头部 + payload CRC + 版本链记录体可解析。
    ///
    /// 返回损坏段的 id 列表(供 `check()` 报告);不修改任何状态。
    pub(crate) fn verify_segments(&self) -> Vec<crate::core::types::SegmentId> {
        let guard = self
            .manifest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut corrupt = Vec::new();
        for segment in &guard.manifest.segments {
            if verify_one_segment(&self.root, segment.segment_id).is_err() {
                corrupt.push(crate::core::types::SegmentId::new(segment.segment_id));
            }
        }
        corrupt
    }

    /// 释放独占锁(`close` 调用;幂等)。
    pub(crate) fn release_lock(&self) {
        let mut guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = None;
    }

    /// 原子写一个库内文件,前置触发 `FsyncHook`(测试崩溃注入)。
    pub(super) fn write_file(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        if let Some(hook) = &self.hook {
            hook.before(crate::persist::hook::IoAction::Write {
                file: rel,
                offset: 0,
                len: bytes.len(),
            })?;
        }
        storage::write_atomic(&self.root, rel, bytes)
    }

    /// 当前 MANIFEST 快照(克隆);供 `snapshot` 子模块使用。
    pub(super) fn manifest_snapshot(&self) -> Manifest {
        self.manifest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .manifest
            .clone()
    }

    /// 当前 `(版本号, MANIFEST)` 快照。
    pub(super) fn versioned_manifest(&self) -> (u64, Manifest) {
        let guard = self
            .manifest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (guard.version, guard.manifest.clone())
    }
}

/// 组装单个活跃段的统计。
fn segment_stat(
    root: &Path,
    segment_id: u32,
    rows: u64,
    created_ms: i64,
) -> crate::memory::ops::SegmentStat {
    // reason: stats 为尽力而为;路径解析/元数据读取失败仅少计字节,不影响正确性。
    let bytes = storage::resolve(root, &format!("{SEGMENTS_DIR}/{}", vsec_name(segment_id)))
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .map_or(0, |metadata| metadata.len());
    crate::memory::ops::SegmentStat {
        id: crate::core::types::SegmentId::new(segment_id),
        rows,
        bytes,
        dead_ratio: 0.0,
        created: created_ms,
    }
}

/// 校验单个段:头部 + payload CRC + 版本链记录体可解析。
fn verify_one_segment(root: &Path, segment_id: u32) -> Result<()> {
    let vsec_bytes =
        storage::read_file(root, &format!("{SEGMENTS_DIR}/{}", vsec_name(segment_id)))?;
    let msec_bytes =
        storage::read_file(root, &format!("{SEGMENTS_DIR}/{}", msec_name(segment_id)))?;
    let mut vsec_view = vsec::parse(&vsec_bytes)?;
    vsec_view.verify_payload()?;
    let mut msec_view = msec::parse(&msec_bytes)?;
    msec_view.verify_payload()?;
    // vsec 与 msec 的行数必须一致(两文件同生同灭,I3)。
    if vsec_view.row_count() != msec_view.row_count() {
        return Err(MnemeError::Corrupted {
            segment: Some(crate::core::types::SegmentId::new(segment_id)),
            reason: "段 vsec/msec 行数不一致".to_string(),
        });
    }
    // key 索引与命名空间统计必须可解析(损坏即报告,不静默)。
    let _ = msec_view.key_rows()?;
    let _ = msec_view.ns_stat_rows()?;
    // 版本链记录体可解析(含 doc_offset 有效性)。
    for row in msec_view.version_rows()? {
        if row.doc_offset != TOMBSTONE_DOC_OFFSET {
            let _ = msec_view.read_entry(row.doc_offset)?;
        }
    }
    Ok(())
}
