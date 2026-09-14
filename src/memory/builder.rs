//! 建库器 `Builder`(`builder.rs`)。
//!
//! 收集建库配置并产出 [`Mneme`](crate::memory::Mneme);字段语义见设计 16 §2。
//! 链式配置 setter 在子模块 [`options`](self::options) 中实现。

use std::sync::Arc;
use std::time::Duration;

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::{
    BuildPrecision, Clock, CompactionPolicy, Compression, Dimension, FsyncPolicy, HnswParams,
    InsertMode, Limits, MonotonicClock, RelationIndex, SystemClock, Tuning, VectorFormat,
};
use crate::memory::config::Config;
use crate::memory::dedup::Dedup;
use crate::memory::engine::Mneme;
use crate::memory::lifecycle::Retention;
use crate::memory::ops::CompactionControl;
use crate::memory::table::{PersistHook, Table, WriterState};
use crate::persist::hook::FsyncHook;
use crate::persist::store::{OpenOptions, Store};

mod options;

/// [`Builder::open_backend`] 的返回:持久后端 + 初始写状态 + 维度 + 度量。
type OpenedBackend = (Option<Arc<Store>>, Option<WriterState>, Dimension, Metric);

/// 近似去重阈值的缺省值(统一按余弦口径)。
const DEFAULT_DEDUP_THRESHOLD: f32 = 0.95;

/// 访问统计落盘周期的缺省值(秒;仅记录,L5 生效)。
const DEFAULT_ACCESS_FLUSH_SECS: u64 = 30;

/// 建库器。
pub struct Builder {
    path: Option<std::path::PathBuf>,
    dimension: Option<u32>,
    metric: Metric,
    metric_explicit: bool,
    fsync: FsyncPolicy,
    insert_mode: InsertMode,
    dedup: Dedup,
    dedup_threshold: f32,
    quantization: VectorFormat,
    hnsw: HnswParams,
    build_precision: BuildPrecision,
    compaction: CompactionPolicy,
    retention: Option<Retention>,
    retain_interval: Option<Duration>,
    access_flush_interval: Duration,
    compression: Compression,
    encryption: Option<crate::crypto::Encryption>,
    observer: Option<Arc<dyn crate::core::observe::Observer>>,
    read_only_probe_interval: std::time::Duration,
    storage: Option<Arc<dyn crate::persist::storage::Storage>>,
    relation_index: RelationIndex,
    parallelism: usize,
    maintenance: bool,
    tuning: Tuning,
    limits: Limits,
    clock: Arc<dyn Clock>,
    read_only: bool,
    verify_on_open: bool,
    fail_fast_on_corruption: bool,
    fsync_hook: Option<Arc<dyn FsyncHook>>,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            path: None,
            dimension: None,
            metric: Metric::Cosine,
            metric_explicit: false,
            fsync: FsyncPolicy::default(),
            insert_mode: InsertMode::default(),
            dedup: Dedup::default(),
            dedup_threshold: DEFAULT_DEDUP_THRESHOLD,
            quantization: VectorFormat::default(),
            hnsw: HnswParams::default(),
            build_precision: BuildPrecision::default(),
            compaction: CompactionPolicy::default(),
            retention: None,
            retain_interval: None,
            access_flush_interval: Duration::from_secs(DEFAULT_ACCESS_FLUSH_SECS),
            compression: Compression::default(),
            encryption: None,
            observer: None,
            read_only_probe_interval: std::time::Duration::from_secs(1),
            storage: None,
            relation_index: RelationIndex::default(),
            parallelism: 0,
            maintenance: true,
            tuning: Tuning::default(),
            limits: Limits::default(),
            clock: Arc::new(SystemClock),
            read_only: false,
            verify_on_open: false,
            fail_fast_on_corruption: false,
            fsync_hook: None,
        }
    }
}

impl std::fmt::Debug for Builder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Builder")
            .field("path", &self.path)
            .field("dimension", &self.dimension)
            .field("metric", &self.metric)
            .finish_non_exhaustive()
    }
}

impl Builder {
    /// 构建库句柄。
    ///
    /// # 既存库配置
    /// 打开既有库时,`Tuning::stopwords` 以 MANIFEST 记录值为准(建库即锁定),
    /// 调用方冲突配置被忽略,避免索引分词与查询分词不一致(FC-PERSIST-POST-009)。
    ///
    /// # Returns
    ///
    /// 打开(或新建)的库句柄 [`Mneme`]。
    ///
    /// # Errors
    /// * 未设置 `dimension`(新建库)→ [`MnemeError::Config`];
    /// * `dedup_threshold` 非 `[0,1]` 内的有限值 → [`MnemeError::Config`]
    ///   (FC-GLOBAL-PRE-004:NaN 会让去重静默失效,越界值超出余弦相似度口径,绝不静默);
    /// * HNSW 参数域非法(`m < 2`/`m0 < m`/`ef_construction = 0`/`ef_search = 0`)或过滤阈值
    ///   非法(非有限值、越界、`brute > post`)→ [`MnemeError::Config`];`ef_search` 或度数
    ///   超上限 → [`MnemeError::LimitExceeded`](FC-INDEX-PRE-001);
    /// * `path` 已存在库且显式维度/度量与其不符 → [`MnemeError::DimensionMismatch`]/
    ///   [`MnemeError::MetricMismatch`];目录被其他实例独占 → [`MnemeError::Busy`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::Builder;
    /// let db = Builder::default().dimension(2).build().unwrap();
    /// assert!(db.list_namespaces().unwrap().is_empty());
    /// ```
    pub fn build(mut self) -> Result<Mneme> {
        self.validate()?;
        let (store, recovered, dimension, metric) = self.open_backend()?;
        // 既存库的分词口径以 MANIFEST 为准(建库即锁定):调用方冲突配置
        // 只影响本次查询会造成索引/查询分词不一致,静默漏召回。
        if let Some(state) = &recovered {
            self.tuning.stopwords = state.stopwords_enabled;
        }
        let read_only_probe_interval = self.read_only_probe_interval;
        // 后台维护开关需在 `into_config` 消费 `self` 之前取出(`FC-LIFE-POST-010`)。
        let maintenance = self.maintenance;
        let config = Arc::new(self.into_config(dimension, metric));
        let hook = store.as_ref();
        let table = build_table(hook, recovered, &config);
        let mut db = Mneme {
            table,
            config,
            control: CompactionControl::new(),
            store,
            maintenance: None,
        };
        // 后台维护:持久库(访问攒批/自动 compaction)或显式开启自动遗忘的纯内存库;
        // `maintenance(false)` 时整体不启动(手动 `maintenance_tick`/`compact` 照常)。
        if maintenance
            && !db.config.read_only
            && (db.store.is_some() || db.config.retention.is_some())
        {
            db.maintenance = Some(crate::life::maintenance::spawn(
                &db.table,
                &db.config,
                &db.control,
                db.store.as_ref(),
            ));
        }
        // 只读共享:周期探测 `current` 并原子换视图(设计 12 §2.1;0 = 关闭)。
        if db.config.read_only
            && !read_only_probe_interval.is_zero()
            && let Some(store) = db.store.as_ref()
        {
            db.maintenance = Some(crate::life::maintenance::spawn_read_only_probe(
                &db.table,
                store,
                read_only_probe_interval,
            ));
        }
        Ok(db)
    }

    /// 校验跨字段配置约束(FC-GLOBAL-PRE-004、FC-INDEX-PRE-001)。
    fn validate(&self) -> Result<()> {
        // NaN 的 `contains` 恒为 false,一个区间判断即可同时覆盖非有限值与越界。
        if !(0.0..=1.0).contains(&self.dedup_threshold) {
            return Err(MnemeError::Config {
                reason: "dedup_threshold 必须是 [0,1] 内的有限值",
            });
        }
        self.validate_hnsw()?;
        self.validate_tuning()?;
        self.validate_compaction()?;
        self.validate_quantization()?;
        self.validate_compression()?;
        crate::crypto::ensure_supported(self.encryption.as_ref())?;
        Ok(())
    }

    /// 校验 compaction 策略(FC-LIFE-PRE-001):分级比/阈值至少 2、段初始行数至少 1、
    /// 死比率与 IO 配额为 `[0,1]` 内有限值,否则触发条件永假或除零。
    fn validate_compaction(&self) -> Result<()> {
        let policy = self.compaction;
        if policy.tier_ratio < 2 || policy.tier_count < 2 {
            return Err(MnemeError::Config {
                reason: "compaction tier_ratio/tier_count 必须 ≥ 2",
            });
        }
        if policy.segment_rows < 1 {
            return Err(MnemeError::Config {
                reason: "compaction segment_rows 必须 ≥ 1",
            });
        }
        if !(0.0..=1.0).contains(&policy.dead_ratio) {
            return Err(MnemeError::Config {
                reason: "compaction dead_ratio 必须是 [0,1] 内的有限值",
            });
        }
        if !(0.0..=1.0).contains(&policy.io_budget) {
            return Err(MnemeError::Config {
                reason: "compaction io_budget 必须是 [0,1] 内的有限值",
            });
        }
        Ok(())
    }

    /// 校验 HNSW 参数:`m ≥ 2`、`m0 ≥ m`、`ef_construction ≥ 1`、`ef_search ∈ [1, ef_max]`、
    /// 度数不超硬上限(FC-INDEX-PRE-001;越限会使自产 hidx 无法读回,或让默认查询宽度
    /// 绕过查询期上限)。
    fn validate_hnsw(&self) -> Result<()> {
        let params = self.hnsw;
        if params.m < 2 || params.m0 < params.m {
            return Err(MnemeError::Config {
                reason: "HNSW m 必须 ≥ 2 且 m0 ≥ m",
            });
        }
        if params.ef_construction < 1 {
            return Err(MnemeError::Config {
                reason: "HNSW ef_construction 必须 ≥ 1",
            });
        }
        if params.ef_search < 1 {
            return Err(MnemeError::Config {
                reason: "HNSW ef_search 必须 ≥ 1",
            });
        }
        let ef_max = self.limits.ef_max as usize;
        if params.ef_search as usize > ef_max {
            return Err(MnemeError::LimitExceeded {
                field: "ef_search",
                limit: ef_max,
                got: params.ef_search as usize,
            });
        }
        let max = crate::memory::index::MAX_INDEX_DEGREE as usize;
        if params.m as usize > max || params.m0 as usize > max {
            return Err(MnemeError::LimitExceeded {
                field: "hnsw 度数(m/m0)",
                limit: max,
                got: params.m.max(params.m0) as usize,
            });
        }
        Ok(())
    }

    /// 校验过滤三档阈值与 bloom/字段上限:有限且 `0 ≤ brute ≤ post ≤ 1`;
    /// `bloom_fpp ∈ (0,1)`(否则会生产 `k > 64`、自读不回的段);
    /// `field_dict_max ≥ 1`(至少容纳 key 字符串字段)(FC-INDEX-PRE-001);
    /// 两阶段候选上限 ≥ 1、召回门槛有限且 ≥ 0(FC-QUANT-PRE-001)。
    fn validate_tuning(&self) -> Result<()> {
        let post = self.tuning.filter_post_threshold;
        let brute = self.tuning.filter_brute_threshold;
        if !(0.0..=1.0).contains(&post) || !(0.0..=1.0).contains(&brute) || brute > post {
            return Err(MnemeError::Config {
                reason: "过滤三档阈值必须是 [0,1] 内有限值且 brute ≤ post",
            });
        }
        let fpp = self.tuning.bloom_fpp;
        if !fpp.is_finite() || fpp <= 0.0 || fpp >= 1.0 {
            return Err(MnemeError::Config {
                reason: "bloom_fpp 必须是 (0,1) 内的有限值",
            });
        }
        if self.tuning.field_dict_max < 1 {
            return Err(MnemeError::Config {
                reason: "field_dict_max 至少为 1(需容纳 key 字段)",
            });
        }
        if self.tuning.rescore_oversample < 1 {
            return Err(MnemeError::Config {
                reason: "rescore_oversample 必须 ≥ 1",
            });
        }
        let floor = self.tuning.quant_recall_floor;
        if !floor.is_finite() || floor < 0.0 {
            return Err(MnemeError::Config {
                reason: "quant_recall_floor 必须是 ≥ 0 的有限值",
            });
        }
        // 建图/建段工程调参:0 会让批行数/切块/线程数失去意义(除零或空批),
        // 一律拒绝(FC-INDEX-PRE-001)。
        if self.tuning.hnsw_compare_cap < 1 {
            return Err(MnemeError::Config {
                reason: "hnsw_compare_cap 必须 ≥ 1",
            });
        }
        if self.tuning.hnsw_batch_rows < 1 {
            return Err(MnemeError::Config {
                reason: "hnsw_batch_rows 必须 ≥ 1",
            });
        }
        if self.tuning.hnsw_serial_rows < 1 {
            return Err(MnemeError::Config {
                reason: "hnsw_serial_rows 必须 ≥ 1",
            });
        }
        if self.tuning.hnsw_threads_max < 1 {
            return Err(MnemeError::Config {
                reason: "hnsw_threads_max 必须 ≥ 1",
            });
        }
        if self.tuning.flush_chunk_rows < 1 {
            return Err(MnemeError::Config {
                reason: "flush_chunk_rows 必须 ≥ 1",
            });
        }
        if self.tuning.flush_threads < 1 {
            return Err(MnemeError::Config {
                reason: "flush_threads 必须 ≥ 1",
            });
        }
        Ok(())
    }

    /// 校验量化配置:feature 门控与纯内存限制(FC-QUANT-ERR-001/002)。
    ///
    /// 量化副本随段同生同灭:纯内存库没有段,配置量化没有可服务的载体,
    /// 构造期即报 `Unsupported`,绝不静默记配置回显 `active = F32`。
    fn validate_quantization(&self) -> Result<()> {
        if self.quantization == VectorFormat::F32 {
            return Ok(());
        }
        crate::quant::ensure_format_supported(self.quantization)?;
        if self.path.is_none() {
            return Err(MnemeError::Unsupported {
                feature: "量化副本(纯内存库无段)",
            });
        }
        Ok(())
    }

    /// 校验压缩策略与 feature 门控:`Lz4` 需 feature `compress`,`Zstd` 需
    /// `compress-zstd`,未开启即 `Unsupported`,绝不静默按 `None` 运行。
    fn validate_compression(&self) -> Result<()> {
        crate::compress::codec_for(self.compression).map(|_codec| ())
    }

    /// 打开持久后端;纯内存库返回 `None` 后端与初始写状态。
    ///
    /// 有 `path` 走持久层;否则纯内存。持久路径先解析维度/度量(已存在库以
    /// MANIFEST 为准),再统一构造配置。
    fn open_backend(&self) -> Result<OpenedBackend> {
        let requested_metric = self.metric_explicit.then_some(self.metric);
        match &self.path {
            Some(path) => {
                let (store, state, dimension, metric) = Store::open(
                    path,
                    OpenOptions {
                        dimension: self.dimension,
                        metric: requested_metric,
                        fsync: self.fsync,
                        read_only: self.read_only,
                        verify_on_open: self.verify_on_open,
                        fail_fast_on_corruption: self.fail_fast_on_corruption,
                        hook: self.fsync_hook.clone(),
                        index_factory: Some(crate::index::default_factory()),
                        wal_file_bytes: self.compaction.wal_file_bytes,
                        tuning: self.tuning.clone(),
                        compression: self.compression,
                        encryption: self.encryption.clone(),
                        storage: self.storage.clone(),
                        observer: self.observer.clone(),
                    },
                )?;
                Ok((Some(store), Some(state), Dimension::new(dimension)?, metric))
            }
            None => {
                let dimension = self.dimension.ok_or(MnemeError::Config {
                    reason: "新建内存库必须指定维度",
                })?;
                Ok((None, None, Dimension::new(dimension)?, self.metric))
            }
        }
    }

    /// 消耗 `Builder` 构造不可变运行配置。
    fn into_config(self, dimension: Dimension, metric: Metric) -> Config {
        Config {
            dimension,
            metric,
            insert_mode: self.insert_mode,
            dedup: self.dedup,
            dedup_threshold: self.dedup_threshold,
            quantization: self.quantization,
            hnsw: self.hnsw,
            build_precision: self.build_precision,
            index_factory: Some(crate::index::default_factory()),
            compaction: self.compaction,
            retention: self.retention,
            retain_interval: self.retain_interval,
            access_flush_interval: self.access_flush_interval,
            compression: self.compression,
            observer: self.observer,
            relation_index: self.relation_index,
            parallelism: self.parallelism,
            tuning: self.tuning,
            limits: self.limits,
            // 时钟回拨单调钳制:TTL 只可能晚消失(FC-GLOBAL-PRE-005)。
            clock: Arc::new(MonotonicClock::new(self.clock)),
            read_only: self.read_only,
        }
    }
}

/// 由持久后端与写状态构造物理表。
fn build_table(
    store: Option<&Arc<Store>>,
    recovered: Option<WriterState>,
    config: &Arc<Config>,
) -> Arc<Table> {
    match (store, recovered) {
        (Some(store), Some(state)) => {
            let hook: Arc<dyn PersistHook> = Arc::clone(store) as Arc<dyn PersistHook>;
            Arc::new(Table::from_state(Arc::clone(config), state, Some(hook)))
        }
        _ => Arc::new(Table::new(Arc::clone(config))),
    }
}
