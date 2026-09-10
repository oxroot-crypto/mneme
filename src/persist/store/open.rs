//! `Store::open` 及身份解析 / 状态载入辅助(`store/open.rs`)。
//!
//! 打开流程:确保目录布局 → 取独占锁 → 载入或初始化 MANIFEST → 载入段并回放
//! WAL 重建写状态 → 打开 WAL 写入器。

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::FsyncPolicy;
use crate::memory::table::WriterState;
use crate::persist::hook::FsyncHook;
use crate::persist::manifest::Manifest;
use crate::persist::recover;
use crate::persist::storage::{self, FileLock, SEGMENTS_DIR, WAL_DIR, msec_name, vsec_name};
use crate::persist::trash;
use crate::persist::wal;

use super::manifest_io;
use super::wal_writer::{WAL_FILE, WalConfig, WalWriter};
use super::{ManifestState, Store};

/// 新建库时首个可分配的关系类型编号(内建关系占用 `0..16`)。
const NEXT_REL_KIND_INITIAL: u16 = 16;

/// `Store::open` 的输入参数。
pub(crate) struct OpenOptions {
    /// 请求维度;`None` 表示沿用已有库。
    pub(crate) dimension: Option<u32>,
    /// 请求度量;`None` 表示沿用已有库。
    pub(crate) metric: Option<Metric>,
    /// fsync 策略。
    pub(crate) fsync: FsyncPolicy,
    /// 只读打开(不持锁、不写盘)。
    pub(crate) read_only: bool,
    /// 打开时校验段 payload CRC。
    pub(crate) verify_on_open: bool,
    /// 段损坏时快速失败(否则隔离跳过)。
    pub(crate) fail_fast_on_corruption: bool,
    /// 崩溃注入钩子。
    pub(crate) hook: Option<Arc<dyn FsyncHook>>,
}

impl Store {
    /// 打开或新建持久库。
    ///
    /// 返回 `(store, 初始写状态, 维度, 度量)`。已存在库以 MANIFEST 的维度/度量为准,
    /// 与调用方请求不符时拒绝打开(设计 16 §3)。
    ///
    /// # Errors
    /// 目录不可用、锁被占、MANIFEST/段损坏(且 fail-fast)或维度冲突时返回结构化错误。
    pub(crate) fn open(
        root: &Path,
        options: OpenOptions,
    ) -> Result<(Arc<Store>, WriterState, u32, Metric)> {
        prepare_dirs(root)?;
        let lock = acquire_lock(root, options.read_only)?;
        trash::purge(root)?;
        // 清理崩溃残留的 `Building` 半成品(ATOMIC 写的 `.tmp`);只读实例亦只读取、不删除。
        if !options.read_only {
            manifest_io::cleanup_orphans(root)?;
        }

        let (manifest, version) = load_or_init_manifest(root, options.dimension, options.metric)?;

        // 可写实例清理 MANIFEST 未引用的段孤儿(garbage),只读实例不写盘。
        if !options.read_only {
            manifest_io::remove_unreferenced_segments(root, &manifest)?;
        }

        let state = load_write_state(root, &manifest, &options)?;
        let wal = WalWriter::open_or_create(root, wal_config(&manifest, &options))?;
        let store = Arc::new(Store {
            root: root.to_path_buf(),
            lock: Mutex::new(lock),
            wal: Mutex::new(wal),
            manifest: Mutex::new(ManifestState {
                version,
                manifest: manifest.clone(),
            }),
            dimension: manifest.dimension,
            metric: manifest.metric,
            read_only: options.read_only,
            hook: options.hook,
        });
        Ok((store, state, manifest.dimension, manifest.metric))
    }
}

/// 确保库根、`segments/`、`wal/` 与 `trash/` 目录存在。
fn prepare_dirs(root: &Path) -> Result<()> {
    storage::ensure_dir(root)?;
    storage::ensure_dir(&root.join(SEGMENTS_DIR))?;
    storage::ensure_dir(&root.join(WAL_DIR))?;
    storage::ensure_dir(&root.join(storage::TRASH_DIR))?;
    Ok(())
}

/// 可写实例取独占锁;只读实例不持锁。
fn acquire_lock(root: &Path, read_only: bool) -> Result<Option<FileLock>> {
    if read_only {
        Ok(None)
    } else {
        Ok(Some(FileLock::acquire(root)?))
    }
}

/// 由 MANIFEST 与打开参数构造 WAL 写入器配置。
fn wal_config(manifest: &Manifest, options: &OpenOptions) -> WalConfig {
    WalConfig {
        dimension: manifest.dimension,
        metric: manifest.metric,
        policy: options.fsync,
        hook: options.hook.clone(),
        read_only: options.read_only,
    }
}

/// 载入既有 MANIFEST,或据请求与 WAL 头初始化一个新 MANIFEST。
fn load_or_init_manifest(
    root: &Path,
    requested_dimension: Option<u32>,
    requested_metric: Option<Metric>,
) -> Result<(Manifest, u64)> {
    let loaded = manifest_io::load_manifest(root)?;
    // 段文件存在却无 MANIFEST:不一致状态,拒绝当作新库覆盖既有数据(设计 16 §3)。
    if loaded.is_none() && !manifest_io::segment_files(root)?.is_empty() {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "段文件存在但无 MANIFEST,拒绝覆盖".to_string(),
        });
    }
    match loaded {
        Some((manifest, version)) => {
            verify_requested_identity(&manifest, requested_dimension, requested_metric)?;
            Ok((manifest, version))
        }
        None => Ok((
            init_manifest_from_wal(root, requested_dimension, requested_metric)?,
            0,
        )),
    }
}

/// 校验调用方请求的维度/度量与既有 MANIFEST 一致。
fn verify_requested_identity(
    manifest: &Manifest,
    requested_dimension: Option<u32>,
    requested_metric: Option<Metric>,
) -> Result<()> {
    if let Some(dimension) = requested_dimension
        && dimension != manifest.dimension
    {
        return Err(MnemeError::DimensionMismatch {
            expected: manifest.dimension,
            got: dimension as usize,
        });
    }
    if let Some(metric) = requested_metric
        && metric != manifest.metric
    {
        return Err(MnemeError::MetricMismatch {
            existing: manifest.metric,
            requested: metric,
        });
    }
    Ok(())
}

/// 无 MANIFEST 但可能有 WAL(崩溃在首次 flush 前):以 WAL 头为准初始化。
fn init_manifest_from_wal(
    root: &Path,
    requested_dimension: Option<u32>,
    requested_metric: Option<Metric>,
) -> Result<Manifest> {
    let wal_header = storage::read_file_opt(root, WAL_FILE)?
        .and_then(|bytes| wal::parse_file_header(&bytes).ok());
    let dimension = resolve_dimension(requested_dimension, wal_header.as_ref())?;
    let metric = resolve_metric(requested_metric, wal_header.as_ref())?;
    Ok(Manifest {
        dimension,
        metric,
        next_rel_kind: NEXT_REL_KIND_INITIAL,
        manifest_version: 0,
        watermark_seqno: 0,
        next_rowid: 0,
        next_segment_id: 0,
        next_ns_id: 1,
        namespaces: Vec::new(),
        rel_kinds: Vec::new(),
        segments: Vec::new(),
    })
}

/// 解析最终维度:WAL 头与请求冲突时拒绝,缺失时要求请求显式指定。
fn resolve_dimension(requested: Option<u32>, wal_header: Option<&wal::WalHeader>) -> Result<u32> {
    match (requested, wal_header) {
        (_, None) => requested.ok_or(MnemeError::Config {
            reason: "新建持久库必须指定维度",
        }),
        (Some(d), Some(header)) if header.dimension != d => Err(MnemeError::DimensionMismatch {
            expected: header.dimension,
            got: d as usize,
        }),
        (Some(d), Some(_)) => Ok(d),
        (None, Some(header)) => Ok(header.dimension),
    }
}

/// 解析最终度量:WAL 头与请求冲突时拒绝,缺失时缺省余弦。
fn resolve_metric(
    requested: Option<Metric>,
    wal_header: Option<&wal::WalHeader>,
) -> Result<Metric> {
    match (requested, wal_header) {
        (_, None) => Ok(requested.unwrap_or(Metric::Cosine)),
        (Some(m), Some(header)) if header.metric != m => Err(MnemeError::MetricMismatch {
            existing: header.metric,
            requested: m,
        }),
        (Some(m), Some(_)) => Ok(m),
        (None, Some(header)) => Ok(header.metric),
    }
}

/// 载入 MANIFEST 所列段并回放 WAL,重建写状态。
///
/// 损坏段(头部不可解析)在非 fail-fast 下被移到 `trash/` 并跳过。
fn load_write_state(
    root: &Path,
    manifest: &Manifest,
    options: &OpenOptions,
) -> Result<WriterState> {
    let mut state = recover::empty_state(manifest);
    let segments = manifest_io::read_segment_bytes(root, manifest)?;
    let skipped = recover::load_segments(
        &mut state,
        &segments,
        options.verify_on_open,
        options.fail_fast_on_corruption,
    )?;
    if !options.read_only && !skipped.is_empty() {
        let names: Vec<String> = skipped
            .iter()
            .flat_map(|id| [vsec_name(*id), msec_name(*id)])
            .collect();
        trash::move_to_trash(root, &names)?;
    }
    // 回放 WAL(仅 seqno > watermark)。
    if let Some(bytes) = storage::read_file_opt(root, WAL_FILE)? {
        recover::replay_wal(&mut state, &bytes, manifest.watermark_seqno)?;
    }
    Ok(state)
}
