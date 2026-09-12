//! 建库器 `Builder`(`builder.rs`)。
//!
//! 收集建库配置并产出 [`Mneme`](crate::memory::Mneme);字段语义见设计 16 §2。
//! 链式配置 setter 在子模块 [`options`](self::options) 中实现。

use std::sync::Arc;
use std::time::Duration;

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::{
    Clock, CompactionPolicy, Compression, Dimension, FsyncPolicy, HnswParams, InsertMode, Limits,
    MonotonicClock, RelationIndex, SystemClock, Tuning, VectorFormat,
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
    compaction: CompactionPolicy,
    retention: Option<Retention>,
    retain_interval: Option<Duration>,
    access_flush_interval: Duration,
    compression: Compression,
    relation_index: RelationIndex,
    parallelism: usize,
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
            compaction: CompactionPolicy::default(),
            retention: None,
            retain_interval: None,
            access_flush_interval: Duration::from_secs(DEFAULT_ACCESS_FLUSH_SECS),
            compression: Compression::default(),
            relation_index: RelationIndex::default(),
            parallelism: 0,
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
        // 后台维护:持久库(访问攒批/自动 compaction)或显式开启自动遗忘的纯内存库。
        if !db.config.read_only && (db.store.is_some() || db.config.retention.is_some()) {
            db.maintenance = Some(crate::life::maintenance::spawn(
                &db.table,
                &db.config,
                &db.control,
                db.store.as_ref(),
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
    /// `field_dict_max ≥ 1`(至少容纳 key 字符串字段)(FC-INDEX-PRE-001)。
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
        Ok(())
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
            index_factory: Some(crate::index::default_factory()),
            compaction: self.compaction,
            retention: self.retention,
            retain_interval: self.retain_interval,
            access_flush_interval: self.access_flush_interval,
            compression: self.compression,
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
