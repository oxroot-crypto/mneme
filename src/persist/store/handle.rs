//! L2 持久化协调句柄 `Store` 的定义与统计/校验辅助(`store/handle.rs`)。
//!
//! `Store` 持有目录、独占锁、WAL 写入器与当前 MANIFEST,实现内存引擎的
//! [`PersistHook`](crate::memory::table::PersistHook);具体的打开/落盘/备份
//! 实现分散在同目录的 `open`/`snapshot`/`hook` 等子模块。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::{Compression, VectorFormat};
use crate::memory::index::{IndexFactory, SegmentIndex};
use crate::persist::hook::FsyncHook;
use crate::persist::manifest::Manifest;
use crate::persist::msec::{self, TOMBSTONE_DOC_OFFSET};
use crate::persist::storage::{SEGMENTS_DIR, hidx_name, msec_name, vsec_name};
use crate::persist::trash;
use crate::persist::vsec;

use super::wal_writer::WalWriter;

/// 当前 MANIFEST 快照(受 `Mutex` 保护)。
pub(crate) struct ManifestState {
    pub(super) version: u64,
    pub(super) manifest: Manifest,
}

/// L2 持久化协调句柄。
pub(crate) struct Store {
    pub(super) root: PathBuf,
    pub(super) lock: Mutex<Option<Box<dyn std::any::Any + Send + Sync>>>,
    /// 存储后端(`FsStorage` 默认;`Builder::storage` 可注入,设计 12 §3.1)。
    pub(super) storage: Arc<dyn crate::persist::storage::Storage>,
    pub(super) wal: Mutex<WalWriter>,
    pub(super) manifest: Mutex<ManifestState>,
    pub(super) dimension: u32,
    pub(super) metric: Metric,
    pub(crate) read_only: bool,
    pub(super) hook: Option<Arc<dyn FsyncHook>>,
    /// 索引工厂(校验 hidx 用;`None` = 不校验)。
    pub(super) index_factory: Option<Arc<dyn IndexFactory>>,
    /// 文本/元数据压缩策略(WAL 记录体编码用;段编码见 `flush`)。
    pub(super) compression: Compression,
    /// 静态加密配置(`None` = 明文;段/WAL/MANIFEST 信封见 `crate::crypto`)。
    pub(super) encryption: Option<crate::crypto::Encryption>,
    /// 事件可观测钩子(`None` = 关闭;设计 12 §4)。
    pub(crate) observer: Option<Arc<dyn crate::core::observe::Observer>>,
    /// 打开时的调优口径(只读重载重建加速结构用)。
    pub(super) tuning: crate::core::options::Tuning,
}

impl Store {
    /// 当前 MANIFEST 版本号。
    pub(crate) fn current_version(&self) -> u64 {
        self.manifest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .version
    }

    /// 只读重载:原子替换 MANIFEST 快照(不写盘)。
    pub(crate) fn replace_manifest(&self, manifest: Manifest, version: u64) {
        let mut guard = self
            .manifest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.manifest = manifest;
        guard.version = version;
    }

    /// 读取 `current` 文件的版本号;不存在/非法返回 `None`。
    pub(crate) fn read_current(&self) -> Option<u64> {
        let bytes = self
            .storage
            .read_file_opt(crate::persist::storage::CURRENT_FILE)
            .ok()??;
        String::from_utf8_lossy(&bytes).trim().parse().ok()
    }

    /// 是否启用静态加密。
    pub(crate) fn encryption_enabled(&self) -> bool {
        self.encryption.is_some()
    }

    /// 加密配置(`None` = 明文);密钥轮换用(共享 provider 的 `Arc`)。
    pub(crate) fn encryption_config(&self) -> Option<&crate::crypto::Encryption> {
        self.encryption.as_ref()
    }

    /// 已迁移到当前 active 密钥的段数(未启用加密时等于总段数)。
    ///
    /// 只读信封头 10 字节(不接触密钥内容);文件缺失/读取失败按未迁移计。
    pub(crate) fn migrated_segments(&self) -> usize {
        let Some(encryption) = self.encryption.as_ref() else {
            return self.total_segments();
        };
        let active = encryption.provider.active_key();
        let segments = self.manifest_snapshot().segments;
        segments
            .iter()
            .filter(|segment| {
                let rel = format!("{SEGMENTS_DIR}/{}", vsec_name(segment.segment_id));
                // reason: 统计为尽力而为;读取失败按未迁移计,不影响正确性。
                // 只读信封头 10 字节,不得为统计整读大段(FC-PERSIST-INV-021)。
                let Ok(head) = self.storage.read_prefix(&rel, 10) else {
                    return false;
                };
                crate::crypto::envelope_key_id(&head) == Some(active.0)
            })
            .count()
    }

    /// 全部 WAL 文件字节数(统计为尽力而为,不阻断调用方)。
    pub(crate) fn wal_bytes(&self) -> u64 {
        // 写入器维护「活动 + 已轮转」字节计数,免每写事务列目录 + 逐文件 stat
        // (WAL 达硬上限前可积累数十个轮转文件)。
        let wal = self
            .wal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        wal.total_bytes()
    }

    /// `trash/` 目录字节数(统计为尽力而为,不可读时返回 0)。
    pub(crate) fn trash_bytes(&self) -> u64 {
        // reason: stats 为尽力而为;trash 读取失败仅少计字节,不影响正确性。
        trash::bytes(self.storage.as_ref()).unwrap_or(0)
    }

    /// 活跃段统计(来自当前 MANIFEST);`indexes` 为真实载入的段索引(用于 HNSW 统计)。
    pub(crate) fn segment_stats(
        &self,
        indexes: &[crate::memory::index::SegmentIndex],
    ) -> Vec<crate::memory::ops::SegmentStat> {
        // 锁内只克隆段元数据,文件 I/O 放到锁外(避免阻塞并发 flush 的 MANIFEST 提交)。
        let segments = self.manifest_snapshot().segments;
        segments
            .iter()
            .map(|segment| {
                // 多段架构:按段号匹配该段真实载入的索引(含量化元信息)。
                let loaded = indexes
                    .iter()
                    .find(|loaded| loaded.segment_id == segment.segment_id);
                segment_stat(self.storage.as_ref(), segment, loaded)
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
        // 锁内只克隆段元数据,校验(读盘 + CRC)放到锁外。
        let segments = self.manifest_snapshot().segments;
        let mut corrupt = Vec::new();
        for segment in &segments {
            if verify_one_segment(
                self.storage.as_ref(),
                segment.segment_id,
                segment.hidx_crc,
                self.index_factory.as_deref(),
                self.encryption.as_ref(),
            )
            .is_err()
            {
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
    pub(super) fn write_file(
        &self,
        rel: &str,
        scope: &[u8],
        identity: u64,
        bytes: &[u8],
    ) -> Result<()> {
        let sealed = crate::crypto::encrypt_file(self.encryption.as_ref(), scope, identity, bytes)?;
        let bytes = sealed.as_ref();
        if let Some(hook) = &self.hook {
            hook.before(crate::persist::hook::IoAction::Write {
                file: rel,
                offset: 0,
                len: bytes.len(),
            })?;
        }
        self.storage.write_atomic(rel, bytes)
    }

    /// 当前 MANIFEST 快照(克隆);供内存门面与 `snapshot` 子模块使用。
    pub(crate) fn manifest_snapshot(&self) -> Manifest {
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

/// 组装单个活跃段的统计(HNSW 字段取自真实载入的索引,未载入则为 0)。
fn segment_stat(
    storage: &dyn crate::persist::storage::Storage,
    segment: &crate::persist::manifest::SegmentEntry,
    loaded: Option<&SegmentIndex>,
) -> crate::memory::ops::SegmentStat {
    // reason: stats 为尽力而为;元数据读取失败仅少计字节,不影响正确性。
    let file_bytes = |name: &str| {
        storage
            .stat(&format!("{SEGMENTS_DIR}/{name}"))
            .map_or(0, |meta| meta.len)
    };
    let mut bytes = file_bytes(&vsec_name(segment.segment_id));
    if segment.hidx_crc != 0 {
        bytes += file_bytes(&hidx_name(segment.segment_id));
    }
    let index = loaded.map(|entry| entry.index.as_ref());
    crate::memory::ops::SegmentStat {
        id: crate::core::types::SegmentId::new(segment.segment_id),
        rows: segment.row_count,
        bytes,
        dead_ratio: 0.0,
        created: segment.created_ms,
        index_nodes: index.map_or(0, |idx| idx.node_count() as u64),
        index_levels: index.map_or(0, |idx| idx.max_level()),
        quant: loaded.map_or(VectorFormat::F32, |entry| entry.quant),
        recall_est: loaded.and_then(|entry| entry.recall_est),
    }
}

/// 校验单个段:头部 + payload CRC + 版本链记录体可解析 + hidx(若有)。
fn verify_one_segment(
    storage: &dyn crate::persist::storage::Storage,
    segment_id: u32,
    hidx_crc: u32,
    factory: Option<&dyn IndexFactory>,
    encryption: Option<&crate::crypto::Encryption>,
) -> Result<()> {
    verify_segment_files(storage, segment_id, encryption)?;
    verify_hidx(storage, segment_id, hidx_crc, factory, encryption)
}

/// 校验 vsec/msec 两个必选段文件:payload CRC、行数一致、索引与版本链可解析。
fn verify_segment_files(
    storage: &dyn crate::persist::storage::Storage,
    segment_id: u32,
    encryption: Option<&crate::crypto::Encryption>,
) -> Result<()> {
    let vsec_bytes = crate::crypto::decrypt_file(
        encryption,
        b"vsec",
        u64::from(segment_id),
        storage.read_file(&format!("{SEGMENTS_DIR}/{}", vsec_name(segment_id)))?,
    )?;
    let msec_bytes = crate::crypto::decrypt_file(
        encryption,
        b"msec",
        u64::from(segment_id),
        storage.read_file(&format!("{SEGMENTS_DIR}/{}", msec_name(segment_id)))?,
    )?;
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

/// 校验可选 hidx:整文件 CRC 与索引内部布局(经 L1 工厂,避免 L2 依赖 L3)。
fn verify_hidx(
    storage: &dyn crate::persist::storage::Storage,
    segment_id: u32,
    hidx_crc: u32,
    factory: Option<&dyn IndexFactory>,
    encryption: Option<&crate::crypto::Encryption>,
) -> Result<()> {
    if hidx_crc == 0 {
        return Ok(());
    }
    let Some(factory) = factory else {
        return Ok(());
    };
    let hidx_bytes = crate::crypto::decrypt_file(
        encryption,
        b"hidx",
        u64::from(segment_id),
        storage.read_file(&format!("{SEGMENTS_DIR}/{}", hidx_name(segment_id)))?,
    )?;
    if crate::persist::crc32(&hidx_bytes) != hidx_crc {
        return Err(MnemeError::Corrupted {
            segment: Some(crate::core::types::SegmentId::new(segment_id)),
            reason: "hidx 文件 CRC 与 MANIFEST 不符".to_string(),
        });
    }
    factory.verify(&hidx_bytes)
}
