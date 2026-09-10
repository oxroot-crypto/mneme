//! L2 持久化协调句柄 `Store`(`store/mod.rs`)。
//!
//! `Store` 持有目录、独占锁、WAL 写入器与当前 MANIFEST,实现内存引擎的
//! [`PersistHook`](crate::memory::table::PersistHook):
//!
//! - 每条写操作由 `Table::write_tx` 交给 [`Store::log`],先追加 WAL 再允许可见
//!   (WAL-before-visible,设计 04 §3.1);多操作批以 `BatchBegin/Commit` 包裹;
//! - [`Store::flush`] 把整个写状态物化为新段并提交新 MANIFEST(全量快照,L2 兜底),
//!   随后重置 WAL(Checkpoint)。
//!
//! > 该模块是对设计 04 §1 模块清单的必要补充(L2 需要一个协调句柄);已同步到
//! > 01 §4 与 04 §1。
//!
//! # 子模块
//!
//! * `wal_writer` —— WAL 写入器(追加 / fsync / Checkpoint 重置)。
//! * `open` —— 打开或新建持久库,解析身份并重建写状态。
//! * `manifest_io` —— MANIFEST 载入 / 提交 / 裁剪与段文件清理。
//! * `snapshot` —— 全量快照 flush 与备份。
//! * `hook` —— [`PersistHook`](crate::memory::table::PersistHook) 实现(WAL 落盘)。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::core::error::Result;
use crate::core::metric::Metric;
use crate::persist::hook::FsyncHook;
use crate::persist::manifest::Manifest;
use crate::persist::msec::{self, TOMBSTONE_DOC_OFFSET};
use crate::persist::storage::{self, FileLock, SEGMENTS_DIR, msec_name, vsec_name};
use crate::persist::trash;
use crate::persist::vsec;

mod hook;
mod manifest_io;
mod open;
mod snapshot;
mod wal_writer;

pub(crate) use open::OpenOptions;

use self::wal_writer::{WAL_FILE, WalWriter};

/// 当前 MANIFEST 快照(受 `Mutex` 保护)。
struct ManifestState {
    version: u64,
    manifest: Manifest,
}

/// L2 持久化协调句柄。
pub(crate) struct Store {
    root: PathBuf,
    lock: Mutex<Option<FileLock>>,
    wal: Mutex<WalWriter>,
    manifest: Mutex<ManifestState>,
    dimension: u32,
    metric: Metric,
    read_only: bool,
    hook: Option<Arc<dyn FsyncHook>>,
}

impl Store {
    /// 库根目录。
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// 当前 WAL 文件字节数。
    pub(crate) fn wal_bytes(&self) -> u64 {
        storage::resolve(&self.root, WAL_FILE)
            .ok()
            .and_then(|path| std::fs::metadata(path).ok())
            .map_or(0, |metadata| metadata.len())
    }

    /// `trash/` 目录字节数。
    pub(crate) fn trash_bytes(&self) -> u64 {
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
    fn write_file(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        if let Some(hook) = &self.hook {
            hook.before(crate::persist::hook::IoAction::Write {
                file: rel,
                offset: 0,
                len: bytes.len(),
            })?;
        }
        storage::write_atomic(&self.root, rel, bytes)
    }
}

/// 组装单个活跃段的统计。
fn segment_stat(
    root: &Path,
    segment_id: u32,
    rows: u64,
    created_ms: i64,
) -> crate::memory::ops::SegmentStat {
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
    // 版本链记录体可解析(含 doc_offset 有效性)。
    for row in msec_view.version_rows()? {
        if row.doc_offset != TOMBSTONE_DOC_OFFSET {
            let _ = msec_view.read_entry(row.doc_offset)?;
        }
    }
    Ok(())
}
