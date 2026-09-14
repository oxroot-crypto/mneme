//! `Store::open` 及身份解析 / 状态载入辅助(`store/open.rs`)。
//!
//! 打开流程:确保目录布局 → 取独占锁 → 载入或初始化 MANIFEST → 载入段并回放
//! WAL 重建写状态 → 打开 WAL 写入器。

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::{Compression, FsyncPolicy, Tuning, VectorFormat};
use crate::core::types::SlotId;
use crate::memory::analysis::{BLOOM_INITIAL_CAPACITY, BloomSet, ZoneIndex};
use crate::memory::index::{IndexFactory, IndexNode, QuantCopy, VectorIndex};
use crate::memory::lazy::{ByteSource, ByteSpan, LazyRows};
use crate::memory::table::WriterState;
use crate::persist::hook::FsyncHook;
use crate::persist::manifest::Manifest;
use crate::persist::recover;
use crate::persist::storage::{SEGMENTS_DIR, WAL_DIR};
use crate::persist::trash;
use crate::persist::vsec;
use crate::persist::wal;

use super::manifest_io;
use super::wal_writer::{self, WalConfig, WalWriter};
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
    /// 文本/元数据压缩策略(记录体编码用)。
    pub(crate) compression: Compression,
    /// 静态加密配置(`None` = 明文;信封读写见 `crypto`)。
    pub(crate) encryption: Option<crate::crypto::Encryption>,
    /// 自定义存储后端(`None` = 默认 `FsStorage`;设计 12 §3.1)。
    pub(crate) storage: Option<Arc<dyn crate::persist::storage::Storage>>,
    /// 事件可观测钩子(`None` = 关闭;设计 12 §4)。
    pub(crate) observer: Option<Arc<dyn crate::core::observe::Observer>>,
    /// 索引工厂(L3);`None` = 不载入 hidx(恒暴力)。
    pub(crate) index_factory: Option<Arc<dyn IndexFactory>>,
    /// WAL 单文件轮转阈值(字节;`0` = 不轮转)。
    pub(crate) wal_file_bytes: u64,
    /// 进阶调参(分词开关 / 字段上限 / bloom 误判率;恢复期重建加速结构用)。
    pub(crate) tuning: Tuning,
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
        let storage = resolve_storage(root, options.storage.clone());
        prepare_root(storage.as_ref(), options.read_only)?;
        let lock = acquire_lock(&storage, options.read_only)?;
        // 清理崩溃残留的 `Building` 半成品(ATOMIC 写的 `.tmp`);只读实例不删除。
        cleanup_residue(storage.as_ref(), options.read_only)?;

        let (manifest, version) = load_or_init_manifest(&storage, &options)?;

        // 可写实例清理 MANIFEST 未引用的段孤儿(garbage),只读实例不写盘。
        if !options.read_only {
            manifest_io::remove_unreferenced_segments(storage.as_ref(), &manifest)?;
        }

        let state = load_write_state(&manifest, &options, &storage)?;
        let wal = WalWriter::open_or_create(Arc::clone(&storage), wal_config(&manifest, &options))?;
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
            compression: options.compression,
            encryption: options.encryption,
            storage,
            observer: options.observer,
            tuning: options.tuning.clone(),
        });
        Ok((store, state, manifest.dimension, manifest.metric))
    }
}

/// 解析存储后端:优先调用方注入,否则默认 [`FsStorage`](设计 12 §3.1)。
fn resolve_storage(
    root: &Path,
    requested: Option<Arc<dyn crate::persist::storage::Storage>>,
) -> Arc<dyn crate::persist::storage::Storage> {
    requested.unwrap_or_else(|| {
        Arc::new(crate::persist::storage::FsStorage::new(root))
            as Arc<dyn crate::persist::storage::Storage>
    })
}

/// 只读实例绝不写盘:只要求库目录已存在;可写实例建立目录布局。
fn prepare_root(storage: &dyn crate::persist::storage::Storage, read_only: bool) -> Result<()> {
    if !read_only {
        return prepare_dirs(storage);
    }
    if !storage.root_exists()? {
        return Err(MnemeError::Config {
            reason: "只读模式要求库目录已存在",
        });
    }
    Ok(())
}

/// 清理崩溃残留的 `Building` 半成品(ATOMIC 写的 `.tmp`);只读实例只读不删除。
fn cleanup_residue(storage: &dyn crate::persist::storage::Storage, read_only: bool) -> Result<()> {
    if read_only {
        return Ok(());
    }
    trash::purge(storage)?;
    manifest_io::cleanup_orphans(storage)?;
    Ok(())
}

/// 确保库根、`segments/`、`wal/` 与 `trash/` 目录存在。
fn prepare_dirs(storage: &dyn crate::persist::storage::Storage) -> Result<()> {
    storage.ensure_dir("")?;
    storage.ensure_dir(SEGMENTS_DIR)?;
    storage.ensure_dir(WAL_DIR)?;
    storage.ensure_dir(crate::persist::storage::TRASH_DIR)?;
    Ok(())
}

/// 可写实例取独占锁;只读实例不持锁。
fn acquire_lock(
    storage: &Arc<dyn crate::persist::storage::Storage>,
    read_only: bool,
) -> Result<Option<Box<dyn std::any::Any + Send + Sync>>> {
    if read_only {
        Ok(None)
    } else {
        Ok(Some(storage.try_lock()?))
    }
}

/// 由 MANIFEST 与打开参数构造 WAL 写入器配置。
fn wal_config(manifest: &Manifest, options: &OpenOptions) -> WalConfig {
    WalConfig {
        dimension: manifest.dimension,
        metric: manifest.metric,
        policy: options.fsync,
        max_file_bytes: options.wal_file_bytes,
        hook: options.hook.clone(),
        read_only: options.read_only,
        encryption: options.encryption.clone(),
    }
}

/// 载入既有 MANIFEST,或据请求与 WAL 头初始化一个新 MANIFEST。
fn load_or_init_manifest(
    storage: &Arc<dyn crate::persist::storage::Storage>,
    options: &OpenOptions,
) -> Result<(Manifest, u64)> {
    let (requested_dimension, requested_metric) = (options.dimension, options.metric);
    let loaded = manifest_io::load_manifest(storage.as_ref(), options.encryption.as_ref())?;
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
            let has_segments = !manifest_io::segment_files(storage.as_ref())?.is_empty();
            if has_segments && !wal_has_frames(storage)? {
                return Err(MnemeError::Corrupted {
                    segment: None,
                    reason: "段文件存在但无 MANIFEST 且 WAL 无可应用帧,拒绝覆盖".to_string(),
                });
            }
            Ok((
                init_manifest_from_wal(
                    storage,
                    requested_dimension,
                    requested_metric,
                    options.tuning.stopwords,
                )?,
                0,
            ))
        }
    }
}

/// WAL 文件集是否含至少一个完整可应用帧(区分「首次 flush 崩溃」与「MANIFEST 丢失」)。
fn wal_has_frames(storage: &Arc<dyn crate::persist::storage::Storage>) -> Result<bool> {
    for rel in wal_writer::wal_files(storage.as_ref())? {
        let bytes = storage.read_file(&rel)?;
        match wal::visit_frames(&bytes, |_, _, _, _| Ok(())) {
            Ok(valid_len) if valid_len > wal::FILE_HEADER_LEN => return Ok(true),
            // 头部损坏视作无可应用帧(来源不明,交由上层拒绝覆盖)。
            _ => {}
        }
    }
    Ok(false)
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

/// 无 MANIFEST 但可能有 WAL(崩溃在首次 flush 前):以最早 WAL 文件头为准初始化。
fn init_manifest_from_wal(
    storage: &Arc<dyn crate::persist::storage::Storage>,
    requested_dimension: Option<u32>,
    requested_metric: Option<Metric>,
    stopwords: bool,
) -> Result<Manifest> {
    let wal_header = wal_writer::wal_files(storage.as_ref())?
        .into_iter()
        .find_map(|rel| {
            storage
                .read_file(&rel)
                .ok()
                .and_then(|bytes| wal::parse_file_header(&bytes).ok())
        });
    let dimension = resolve_dimension(requested_dimension, wal_header.as_ref())?;
    let metric = resolve_metric(requested_metric, wal_header.as_ref())?;
    Ok(Manifest {
        dimension,
        metric,
        stopwords,
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

/// 只读实例重载:发现更新的已提交 MANIFEST 时重建写状态快照。
///
/// 返回 `Some((state, version))` 表示切换(`current` 或 MANIFEST 版本更新);
/// 无新版本返回 `None`。调用方以 `Table::publish` 原子换视图,旧视图由 `Arc`
/// 自然退役(I29/FC-DEPLOY-STA-001)。
///
/// # Errors
/// 新 MANIFEST/段损坏时返回结构化错误(只读实例保持旧视图,不中断服务)。
pub(crate) fn reload_read_only(store: &Store) -> Result<Option<(WriterState, u64)>> {
    if !store.read_only {
        return Err(MnemeError::Config {
            reason: "仅只读实例支持 reload",
        });
    }
    let Some(observed) = store.read_current() else {
        return Ok(None);
    };
    if observed <= store.current_version() {
        return Ok(None);
    }
    let Some((manifest, version)) =
        manifest_io::load_manifest(store.storage.as_ref(), store.encryption.as_ref())?
    else {
        return Ok(None);
    };
    if version <= store.current_version() {
        return Ok(None);
    }
    let options = OpenOptions {
        dimension: None,
        metric: None,
        fsync: FsyncPolicy::Never,
        read_only: true,
        verify_on_open: false,
        fail_fast_on_corruption: false,
        hook: None,
        index_factory: store.index_factory.clone(),
        wal_file_bytes: 0,
        tuning: store.tuning.clone(),
        compression: store.compression,
        encryption: store.encryption.clone(),
        storage: Some(Arc::clone(&store.storage)),
        observer: store.observer.clone(),
    };
    let state = load_write_state(&manifest, &options, &store.storage)?;
    store.replace_manifest(manifest, version);
    Ok(Some((state, version)))
}

/// 载入 MANIFEST 所列段并回放 WAL,重建写状态。
///
/// 损坏段(头部/区级结构不可解析)在非 fail-fast 下仅内存跳过,文件原地保留
/// (MANIFEST 仍引用,移动会使后续打开拒启)。
fn load_write_state(
    manifest: &Manifest,
    options: &OpenOptions,
    storage: &Arc<dyn crate::persist::storage::Storage>,
) -> Result<WriterState> {
    let mut state = recover::empty_state(manifest)?;
    prepare_rebuild_structures(&mut state, manifest, options);
    let segments = manifest_io::open_segment_handles(
        storage,
        manifest,
        options.fail_fast_on_corruption,
        options.encryption.as_ref(),
    )?;
    let recovered = recover::load_segments(
        &mut state,
        &segments,
        options.verify_on_open,
        options.fail_fast_on_corruption,
    )?;
    // 损坏段保持在原地、仅在内存跳过:MANIFEST 仍引用它们,绝不自动移到
    // `trash/`(否则下次打开会因"引用段缺失"拒绝启动,隔离变成删数据)。
    // 段数据可能仍可人工修复,`check()` 会报告损坏段。
    if let Some(factory) = options.index_factory.as_ref() {
        state.indexes = load_hidx_indexes(
            &state,
            &HidxxLoadInput {
                segments: &segments,
                recovered: &recovered,
                options,
                factory,
                metric: manifest.metric,
            },
        )?;
    }
    replay_all_wal(storage.as_ref(), &mut state, manifest, options)?;
    Ok(state)
}

/// 初始化恢复期重建加速结构所需的配置口径。
///
/// 必须与建库配置同口径(分词/字段上限/bloom 误判率):停用词开关以 MANIFEST 为准
/// (建库即锁定),否则查询分词与索引分词不一致会静默漏召回。
fn prepare_rebuild_structures(state: &mut WriterState, manifest: &Manifest, options: &OpenOptions) {
    state.stopwords_enabled = manifest.stopwords;
    state.index_fields_max = options.tuning.field_dict_max as usize;
    state.bloom_fpp = options.tuning.bloom_fpp;
    state.zones = Arc::new(ZoneIndex::new(options.tuning.field_dict_max as usize));
    state.key_bloom = Arc::new(BloomSet::new(
        BLOOM_INITIAL_CAPACITY,
        options.tuning.bloom_fpp,
    ));
}

/// [`load_hidx_indexes`] 的输入(参数收敛)。
struct HidxxLoadInput<'a> {
    /// 各段字节(含可选 hidx)。
    segments: &'a [recover::SegmentBytes],
    /// 恢复结果(重排映射与跳过段)。
    recovered: &'a recover::RecoveredSegments,
    /// 打开选项(fail-fast 等)。
    options: &'a OpenOptions,
    /// 图构建工厂。
    factory: &'a Arc<dyn IndexFactory>,
    /// 距离度量。
    metric: Metric,
}

/// 载入各段 hidx 并安装为多段索引(索引是优化:损坏时降级暴力,`check()` 报告)。
///
/// 各段相互独立:段数 ≥ 2 时按可用核数并行载入(打开 1M 多段库时 hidx 卸载
/// 与量化副本解析可线性摊薄),结果按段序回收。
fn load_hidx_indexes(
    state: &WriterState,
    input: &HidxxLoadInput<'_>,
) -> Result<Arc<Vec<crate::memory::index::SegmentIndex>>> {
    // 先收集待处理段(跳过损坏段与缺失重排映射者),保持输入顺序。
    let mut jobs: Vec<(&recover::SegmentBytes, &recover::SegmentRemap)> = Vec::new();
    for segment in input.segments {
        if input.recovered.skipped.contains(&segment.segment_id) {
            continue;
        }
        let Some(remap) = input
            .recovered
            .remaps
            .iter()
            .find(|remap| remap.segment_id == segment.segment_id)
        else {
            continue;
        };
        jobs.push((segment, remap));
    }
    let workers = if cfg!(feature = "wasm") {
        1
    } else {
        std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .min(jobs.len())
    };
    if workers <= 1 {
        let mut indexes = Vec::new();
        for (segment, remap) in jobs {
            if let Some(index) = build_segment_index(state, input, segment, remap)? {
                indexes.push(index);
            }
        }
        return Ok(Arc::new(indexes));
    }
    let chunk_size = jobs.len().div_ceil(workers);
    let pieces: Vec<Result<Vec<(usize, crate::memory::index::SegmentIndex)>>> =
        std::thread::scope(|scope| {
            let handles: Vec<_> = jobs
                .chunks(chunk_size)
                .enumerate()
                .map(|(offset, chunk)| {
                    scope.spawn(move || {
                        let mut out = Vec::with_capacity(chunk.len());
                        for (index, (segment, remap)) in chunk.iter().enumerate() {
                            if let Some(built) = build_segment_index(state, input, segment, remap)?
                            {
                                out.push((offset * chunk_size + index, built));
                            }
                        }
                        Ok(out)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    // reason: 载入为只读解析,线程 panic 只可能来自实现 bug;显式转内部
                    // 不一致错误而非二次 panic(绝不静默少载段)。
                    handle.join().unwrap_or(Err(MnemeError::Inconsistent {
                        reason: "hidx 载入线程 panic",
                    }))
                })
                .collect()
        });
    let mut indexed: Vec<Option<crate::memory::index::SegmentIndex>> =
        (0..jobs.len()).map(|_| None).collect();
    for piece in pieces {
        for (job_index, index) in piece? {
            indexed[job_index] = Some(index);
        }
    }
    Ok(Arc::new(indexed.into_iter().flatten().collect()))
}

/// 载入单个段的 hidx 索引;无 hidx 或损坏且非 fail-fast 时返回 `None`
/// (索引是查询加速器而非数据来源,`db.check()` 会校验并报告)。
///
/// # Errors
/// 量化副本解析失败,或索引载入失败且 `fail_fast_on_corruption` 为真时
/// 返回结构化错误。
fn build_segment_index(
    state: &WriterState,
    input: &HidxxLoadInput<'_>,
    segment: &recover::SegmentBytes,
    remap: &recover::SegmentRemap,
) -> Result<Option<crate::memory::index::SegmentIndex>> {
    let Some(hidx) = segment.hidx.as_ref() else {
        return Ok(None);
    };
    let quant = load_quant_copy(segment)?;
    let format = quant.as_ref().map_or(VectorFormat::F32, |copy| copy.format);
    let loaded = load_index(
        input.factory,
        hidx,
        SlotRemap {
            state,
            remap: &remap.remap,
        },
        input.metric,
        quant,
    );
    match loaded {
        Ok(index) => {
            let slots: Vec<SlotId> = remap
                .remap
                .iter()
                .map(|&global| SlotId::new(global))
                .collect();
            Ok(Some(crate::memory::index::SegmentIndex::new(
                segment.segment_id,
                index,
                slots,
                format,
                None,
            )))
        }
        Err(error) if input.options.fail_fast_on_corruption => Err(error),
        // reason: hidx 损坏时降级为暴力扫描仍然正确,`db.check()` 会校验 hidx 字节
        // 并报告损坏;节点数不匹配的降级可由 `stats().segments[*].index_nodes == 0`
        // 观测,绝不静默丢数据。
        Err(_) => Ok(None),
    }
}

/// 从段 vsec 还原量化副本(行顺序 = 段内槽位顺序 = hidx 节点顺序)。
///
/// 码流以惰性行区挂段句柄(不拷贝码字节;FC-PERSIST-INV-021),首次粗排时按需切片。
///
/// # Errors
/// vsec 解析失败、行数与索引不一致,或 f16 段在未开 `quant-f16` 的构建上打开时
/// 返回结构化错误(FC-QUANT-ERR-002)。
fn load_quant_copy(segment: &recover::SegmentBytes) -> Result<Option<QuantCopy>> {
    let view = vsec::parse(segment.vsec_bytes()?)?;
    let format = view.quant();
    if format == VectorFormat::F32 {
        return Ok(None);
    }
    crate::quant::ensure_format_supported(format)?;
    let node_count = view.row_count() as usize;
    let corrupt = |reason: &'static str| MnemeError::Corrupted {
        segment: Some(crate::core::types::SegmentId::new(segment.segment_id)),
        reason: reason.to_string(),
    };
    let (offset, stride) = view
        .quant_region()
        .ok_or_else(|| corrupt("vsec: 量化副本区缺失"))?;
    let byte_len = stride
        .checked_mul(node_count)
        .ok_or_else(|| corrupt("vsec: 量化副本区长度溢出"))?;
    let source = Arc::clone(&segment.vsec) as Arc<dyn crate::memory::lazy::ByteSource>;
    let span = crate::memory::lazy::ByteSpan::new(source, offset, byte_len)
        .ok_or_else(|| corrupt("vsec: 量化副本区越界"))?;
    let rows =
        LazyRows::new(span, stride, node_count).ok_or_else(|| corrupt("vsec: 量化副本行区不符"))?;
    Ok(Some(QuantCopy {
        format,
        params: view.quant_params(),
        rows,
    }))
}

/// 回放全部 WAL 文件(仅 seqno > watermark),并截断最后一个文件的撕裂尾部。
fn replay_all_wal(
    storage: &dyn crate::persist::storage::Storage,
    state: &mut WriterState,
    manifest: &Manifest,
    options: &OpenOptions,
) -> Result<()> {
    let wal_files = wal_writer::wal_files(storage)?;
    for (position, rel) in wal_files.iter().enumerate() {
        let bytes = storage.read_file(rel)?;
        let valid_len = recover::replay_wal(
            state,
            &bytes,
            manifest.watermark_seqno,
            options.encryption.as_ref(),
        )?;
        // 撕裂帧之后的字节会永久屏蔽后续追加,必须物理截断后再复用该 WAL;
        // 只有最后一个文件可能带撕裂尾(轮转前该文件已完整 fsync)。
        if position + 1 == wal_files.len() && !options.read_only && valid_len < bytes.len() {
            storage.truncate(rel, valid_len as u64)?;
        }
    }
    Ok(())
}

/// [`load_index`] 的槽位来源:恢复后的写状态与"段内槽位 → 全局槽位"重排映射。
struct SlotRemap<'a> {
    /// 恢复后的写状态(hidx 节点按段内顺序取 `rowid`/向量)。
    state: &'a WriterState,
    /// 段内节点 id → 全局槽位。
    remap: &'a [u32],
}

/// 由 hidx 字节与恢复出的槽位构建索引。
///
/// # Errors
/// hidx 解析失败(损坏/版本不一致)或重排映射越界时返回结构化错误。
fn load_index(
    factory: &Arc<dyn IndexFactory>,
    hidx: &Arc<crate::persist::source::ByteFile>,
    slots: SlotRemap<'_>,
    metric: Metric,
    quant: Option<QuantCopy>,
) -> Result<Arc<dyn VectorIndex>> {
    let source = Arc::clone(hidx) as Arc<dyn ByteSource>;
    let span = ByteSpan::new(source, 0, hidx.len()).ok_or_else(|| MnemeError::Corrupted {
        segment: None,
        reason: "hidx: 句柄区间越界".to_string(),
    })?;
    let mut nodes = Vec::with_capacity(slots.remap.len());
    let mut slot_of = Vec::with_capacity(slots.remap.len());
    for &global in slots.remap {
        let slot = slots
            .state
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
    factory.load(&span, &nodes, &slot_of, metric, quant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::heap::TopK;
    use crate::core::types::RowId;

    /// 桩索引:测试只验证 `load_index` 的重排校验,本桩不会被真正调用。
    struct StubIndex;

    impl VectorIndex for StubIndex {
        fn node_count(&self) -> usize {
            0
        }

        fn max_level(&self) -> u8 {
            0
        }

        fn entry(&self) -> (SlotId, u8) {
            (SlotId::new(0), 0)
        }

        fn serialize(&self) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }

        fn search(&self, _params: &crate::memory::index::IndexSearch<'_>) -> TopK<(RowId, SlotId)> {
            TopK::new(0, Metric::Dot)
        }
    }

    /// 桩工厂:不接触 `crate::index` 具体实现,保持 L2 不依赖 L3。
    struct StubFactory;

    impl IndexFactory for StubFactory {
        fn build(
            &self,
            _request: crate::memory::index::IndexBuildRequest<'_>,
        ) -> Result<Arc<dyn VectorIndex>> {
            Ok(Arc::new(StubIndex))
        }

        fn verify(&self, _bytes: &[u8]) -> Result<()> {
            Ok(())
        }

        fn load(
            &self,
            _span: &ByteSpan,
            _nodes: &[IndexNode],
            _slot_of: &[SlotId],
            _metric: Metric,
            _quant: Option<QuantCopy>,
        ) -> Result<Arc<dyn VectorIndex>> {
            Ok(Arc::new(StubIndex))
        }
    }

    /// FC-PERSIST-ERR-009:载入期二次校验——重排映射指向不存在的槽位 → `Corrupted`,
    /// 绝不静默映射到槽位 0(空状态上 `remap = [0]` 即越界)。
    #[test]
    fn load_index_rejects_remap_past_state_slots() {
        let state = WriterState::new();
        let factory: Arc<dyn IndexFactory> = Arc::new(StubFactory);
        let hidx = crate::persist::source::ByteFile::from_bytes(0, vec![1]);
        let error = load_index(
            &factory,
            &hidx,
            SlotRemap {
                state: &state,
                remap: &[0],
            },
            Metric::Dot,
            None,
        )
        .err()
        .expect("重排映射越界必须拒绝载入");
        assert!(matches!(error, MnemeError::Corrupted { .. }));
    }
}
