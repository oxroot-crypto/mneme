//! `Store::open` 及身份解析 / 状态载入辅助(`store/open.rs`)。
//!
//! 打开流程:确保目录布局 → 取独占锁 → 载入或初始化 MANIFEST → 载入段并回放
//! WAL 重建写状态 → 打开 WAL 写入器。

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::FsyncPolicy;
use crate::core::types::SlotId;
use crate::memory::index::{IndexFactory, IndexNode, VectorIndex};
use crate::memory::table::WriterState;
use crate::persist::hook::FsyncHook;
use crate::persist::manifest::Manifest;
use crate::persist::recover;
use crate::persist::storage::{
    self, FileLock, SEGMENTS_DIR, WAL_DIR, hidx_name, msec_name, vsec_name,
};
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
    /// 索引工厂(L3);`None` = 不载入 hidx(恒暴力)。
    pub(crate) index_factory: Option<Arc<dyn IndexFactory>>,
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
        // 只读实例绝不写盘:不建目录、不清 trash、不删孤儿,只要求库目录已存在。
        if options.read_only {
            if !storage::exists(root)? {
                return Err(MnemeError::Config {
                    reason: "只读模式要求库目录已存在",
                });
            }
        } else {
            prepare_dirs(root)?;
        }
        let lock = acquire_lock(root, options.read_only)?;
        // 清理崩溃残留的 `Building` 半成品(ATOMIC 写的 `.tmp`);只读实例亦只读取、不删除。
        if !options.read_only {
            trash::purge(root)?;
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
            index_factory: options.index_factory,
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
    match loaded {
        Some((manifest, version)) => {
            verify_requested_identity(&manifest, requested_dimension, requested_metric)?;
            Ok((manifest, version))
        }
        None => {
            // 无任何合法 MANIFEST:
            // - 段文件存在且 WAL 含可应用帧 → 「首次 flush 中途崩溃」,段是未提交孤儿,
            //   以 WAL 为准重建、随后清理孤儿段,绝不因此拒绝打开而丢数据;
            // - 段文件存在但 WAL 无可应用帧(无 WAL / 空 WAL / 仅头)→ 来源不明
            //   (可能是 MANIFEST 丢失),拒绝当作新库覆盖(设计 16 §3)。
            let has_segments = !manifest_io::segment_files(root)?.is_empty();
            if has_segments && !wal_has_frames(root)? {
                return Err(MnemeError::Corrupted {
                    segment: None,
                    reason: "段文件存在但无 MANIFEST 且 WAL 无可应用帧,拒绝覆盖".to_string(),
                });
            }
            Ok((
                init_manifest_from_wal(root, requested_dimension, requested_metric)?,
                0,
            ))
        }
    }
}

/// WAL 是否含至少一个完整可应用帧(用于区分「首次 flush 崩溃」与「MANIFEST 丢失」)。
fn wal_has_frames(root: &Path) -> Result<bool> {
    let Some(bytes) = storage::read_file_opt(root, WAL_FILE)? else {
        return Ok(false);
    };
    match wal::visit_frames(&bytes, |_, _, _, _| Ok(())) {
        Ok(valid_len) => Ok(valid_len > wal::FILE_HEADER_LEN),
        // 头部损坏视作无可应用帧(来源不明,交由上层拒绝覆盖)。
        Err(_) => Ok(false),
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
    let segments =
        manifest_io::read_segment_bytes(root, manifest, options.fail_fast_on_corruption)?;
    let recovered = recover::load_segments(
        &mut state,
        &segments,
        options.verify_on_open,
        options.fail_fast_on_corruption,
    )?;
    if !options.read_only && !recovered.skipped.is_empty() {
        let names: Vec<String> = recovered
            .skipped
            .iter()
            .flat_map(|id| [vsec_name(*id), msec_name(*id), hidx_name(*id)])
            .collect();
        trash::move_to_trash(root, &names)?;
    }
    // 载入 hidx 并安装到写状态(索引是优化:损坏时降级暴力,`check()` 报告)。
    if let Some(factory) = options.index_factory.as_ref()
        && let Some(remap) = recovered.remap.as_ref()
        && let Some(hidx) = segments.first().and_then(|segment| segment.hidx.as_ref())
    {
        match load_index(factory, hidx, &state, remap, manifest.metric) {
            Ok(index) => state.index = Some(index),
            Err(error) if options.fail_fast_on_corruption => return Err(error),
            // reason: 索引是查询加速器而非数据来源;hidx 损坏时降级为暴力扫描仍然正确,
            // 且 `db.check()` 会通过 `verify_segments` 报告该段损坏,绝不静默丢数据。
            Err(_) => {}
        }
    }
    // 回放 WAL(仅 seqno > watermark),并在可写打开时截断撕裂尾部。
    if let Some(bytes) = storage::read_file_opt(root, WAL_FILE)? {
        let valid_len = recover::replay_wal(&mut state, &bytes, manifest.watermark_seqno)?;
        // 撕裂帧之后的字节会永久屏蔽后续追加,必须物理截断后再复用该 WAL。
        if !options.read_only && valid_len < bytes.len() {
            storage::truncate(root, WAL_FILE, valid_len as u64)?;
        }
    }
    Ok(state)
}

/// 由 hidx 字节与恢复出的槽位构建索引。
///
/// # Errors
/// hidx 解析失败(损坏/版本过高)或重排映射越界时返回结构化错误。
fn load_index(
    factory: &Arc<dyn IndexFactory>,
    hidx: &[u8],
    state: &WriterState,
    remap: &[u32],
    metric: Metric,
) -> Result<Arc<dyn VectorIndex>> {
    let mut nodes = Vec::with_capacity(remap.len());
    let mut slot_of = Vec::with_capacity(remap.len());
    for &global in remap {
        let slot = state
            .slots
            .get(global as usize)
            .ok_or_else(|| MnemeError::Corrupted {
                segment: None,
                reason: "hidx: 重排映射指向不存在的槽位".to_string(),
            })?;
        nodes.push(IndexNode {
            rowid: slot.rowid,
            vector: Arc::clone(&slot.vector),
            norm_sq: slot.norm_sq,
        });
        slot_of.push(SlotId::new(global));
    }
    factory.load(hidx, &nodes, &slot_of, metric)
}
