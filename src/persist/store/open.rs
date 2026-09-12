//! `Store::open` 及身份解析 / 状态载入辅助(`store/open.rs`)。
//!
//! 打开流程:确保目录布局 → 取独占锁 → 载入或初始化 MANIFEST → 载入段并回放
//! WAL 重建写状态 → 打开 WAL 写入器。

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::{FsyncPolicy, Tuning};
use crate::core::types::SlotId;
use crate::memory::analysis::{BLOOM_INITIAL_CAPACITY, BloomSet, ZoneIndex};
use crate::memory::index::{IndexFactory, IndexNode, VectorIndex};
use crate::memory::table::WriterState;
use crate::persist::hook::FsyncHook;
use crate::persist::manifest::Manifest;
use crate::persist::recover;
use crate::persist::storage::{self, FileLock, SEGMENTS_DIR, WAL_DIR};
use crate::persist::trash;
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

        let (manifest, version) = load_or_init_manifest(root, &options)?;

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
        max_file_bytes: options.wal_file_bytes,
        hook: options.hook.clone(),
        read_only: options.read_only,
    }
}

/// 载入既有 MANIFEST,或据请求与 WAL 头初始化一个新 MANIFEST。
fn load_or_init_manifest(root: &Path, options: &OpenOptions) -> Result<(Manifest, u64)> {
    let (requested_dimension, requested_metric) = (options.dimension, options.metric);
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
                init_manifest_from_wal(
                    root,
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
fn wal_has_frames(root: &Path) -> Result<bool> {
    for rel in wal_writer::wal_files(root)? {
        let bytes = storage::read_file(root, &rel)?;
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
    root: &Path,
    requested_dimension: Option<u32>,
    requested_metric: Option<Metric>,
    stopwords: bool,
) -> Result<Manifest> {
    let wal_header = wal_writer::wal_files(root)?.into_iter().find_map(|rel| {
        storage::read_file(root, &rel)
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

/// 载入 MANIFEST 所列段并回放 WAL,重建写状态。
///
/// 损坏段(头部/区级结构不可解析)在非 fail-fast 下仅内存跳过,文件原地保留
/// (MANIFEST 仍引用,移动会使后续打开拒启)。
fn load_write_state(
    root: &Path,
    manifest: &Manifest,
    options: &OpenOptions,
) -> Result<WriterState> {
    let mut state = recover::empty_state(manifest);
    prepare_rebuild_structures(&mut state, manifest, options);
    let segments =
        manifest_io::read_segment_bytes(root, manifest, options.fail_fast_on_corruption)?;
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
    replay_all_wal(&mut state, root, manifest, options)?;
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
fn load_hidx_indexes(
    state: &WriterState,
    input: &HidxxLoadInput<'_>,
) -> Result<Arc<Vec<crate::memory::index::SegmentIndex>>> {
    let mut indexes = Vec::new();
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
        let Some(hidx) = segment.hidx.as_ref() else {
            continue;
        };
        match load_index(
            input.factory,
            hidx,
            SlotRemap {
                state,
                remap: &remap.remap,
            },
            input.metric,
        ) {
            Ok(index) => {
                let slots: Vec<SlotId> = remap
                    .remap
                    .iter()
                    .map(|&global| SlotId::new(global))
                    .collect();
                indexes.push(crate::memory::index::SegmentIndex::new(
                    segment.segment_id,
                    index,
                    slots,
                ));
            }
            Err(error) if input.options.fail_fast_on_corruption => return Err(error),
            // reason: 索引是查询加速器而非数据来源;hidx 损坏时降级为暴力扫描仍然
            // 正确,`db.check()` 会校验 hidx 字节并报告损坏;节点数不匹配的降级可由
            // `stats().segments[*].index_nodes == 0` 观测,绝不静默丢数据。
            Err(_) => {}
        }
    }
    Ok(Arc::new(indexes))
}

/// 回放全部 WAL 文件(仅 seqno > watermark),并截断最后一个文件的撕裂尾部。
fn replay_all_wal(
    state: &mut WriterState,
    root: &Path,
    manifest: &Manifest,
    options: &OpenOptions,
) -> Result<()> {
    let wal_files = wal_writer::wal_files(root)?;
    for (position, rel) in wal_files.iter().enumerate() {
        let bytes = storage::read_file(root, rel)?;
        let valid_len = recover::replay_wal(state, &bytes, manifest.watermark_seqno)?;
        // 撕裂帧之后的字节会永久屏蔽后续追加,必须物理截断后再复用该 WAL;
        // 只有最后一个文件可能带撕裂尾(轮转前该文件已完整 fsync)。
        if position + 1 == wal_files.len() && !options.read_only && valid_len < bytes.len() {
            storage::truncate(root, rel, valid_len as u64)?;
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
    hidx: &[u8],
    slots: SlotRemap<'_>,
    metric: Metric,
) -> Result<Arc<dyn VectorIndex>> {
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
    factory.load(hidx, &nodes, &slot_of, metric)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::heap::TopK;
    use crate::core::options::HnswParams;
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
            _nodes: &[IndexNode],
            _slot_of: &[SlotId],
            _params: HnswParams,
            _metric: Metric,
        ) -> Arc<dyn VectorIndex> {
            Arc::new(StubIndex)
        }

        fn verify(&self, _bytes: &[u8]) -> Result<()> {
            Ok(())
        }

        fn load(
            &self,
            _bytes: &[u8],
            _nodes: &[IndexNode],
            _slot_of: &[SlotId],
            _metric: Metric,
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
        let error = load_index(
            &factory,
            &[],
            SlotRemap {
                state: &state,
                remap: &[0],
            },
            Metric::Dot,
        )
        .err()
        .expect("重排映射越界必须拒绝载入");
        assert!(matches!(error, MnemeError::Corrupted { .. }));
    }
}
