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
    RelationIndex, SystemClock, Tuning, VectorFormat,
};
use crate::memory::config::Config;
use crate::memory::dedup::Dedup;
use crate::memory::engine::Mneme;
use crate::memory::lifecycle::Retention;
use crate::memory::ops::CompactionControl;
use crate::memory::table::{PersistHook, Table};
use crate::persist::hook::FsyncHook;
use crate::persist::store::Store;

mod options;

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
    /// # Errors
    /// * 未设置 `dimension`(新建库)→ [`MnemeError::Config`];
    /// * `dedup_threshold` 非 `[0,1]` 内的有限值 → [`MnemeError::Config`]
    ///   (FC-GLOBAL-PRE-004:NaN 会让去重静默失效,越界值超出余弦相似度口径,绝不静默);
    /// * `path` 已存在库且显式维度/度量与其不符 → [`MnemeError::DimensionMismatch`]/
    ///   [`MnemeError::MetricMismatch`];目录被其他实例独占 → [`MnemeError::Busy`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::Builder;
    /// let db = Builder::default().dimension(2).build().unwrap();
    /// assert!(db.list_namespaces().unwrap().is_empty());
    /// ```
    pub fn build(self) -> Result<Mneme> {
        // NaN 的 `contains` 恒为 false,一个区间判断即可同时覆盖非有限值与越界。
        if !(0.0..=1.0).contains(&self.dedup_threshold) {
            return Err(MnemeError::Config {
                reason: "dedup_threshold 必须是 [0,1] 内的有限值",
            });
        }
        // 有 `path` 走持久层;否则纯内存。持久路径先解析维度/度量(已存在库以
        // MANIFEST 为准),再统一构造配置。
        let requested_metric = self.metric_explicit.then_some(self.metric);
        let (store, recovered, dimension, metric) = match &self.path {
            Some(path) => {
                let (store, state, dimension, metric) = Store::open(
                    path,
                    self.dimension,
                    requested_metric,
                    self.fsync,
                    self.read_only,
                    self.verify_on_open,
                    self.fail_fast_on_corruption,
                    self.fsync_hook.clone(),
                )?;
                (Some(store), Some(state), dimension, metric)
            }
            None => {
                let dimension = self.dimension.ok_or(MnemeError::Config {
                    reason: "新建内存库必须指定维度",
                })?;
                (None, None, dimension, self.metric)
            }
        };
        let dimension = Dimension::new(dimension)?;
        let config = Arc::new(Config {
            dimension,
            metric,
            fsync: self.fsync,
            insert_mode: self.insert_mode,
            dedup: self.dedup,
            dedup_threshold: self.dedup_threshold,
            quantization: self.quantization,
            hnsw: self.hnsw,
            compaction: self.compaction,
            retention: self.retention,
            retain_interval: self.retain_interval,
            access_flush_interval: self.access_flush_interval,
            compression: self.compression,
            relation_index: self.relation_index,
            parallelism: self.parallelism,
            tuning: self.tuning,
            limits: self.limits,
            clock: self.clock,
            read_only: self.read_only,
            verify_on_open: self.verify_on_open,
            fail_fast_on_corruption: self.fail_fast_on_corruption,
        });
        let table = match (&store, recovered) {
            (Some(store), Some(state)) => {
                let hook: Arc<dyn PersistHook> = Arc::clone(store) as Arc<dyn PersistHook>;
                Arc::new(Table::from_state(Arc::clone(&config), state, Some(hook)))
            }
            _ => Arc::new(Table::new(Arc::clone(&config))),
        };
        Ok(Mneme {
            table,
            config,
            control: CompactionControl::new(),
            store,
        })
    }
}
