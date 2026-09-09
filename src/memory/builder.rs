//! 建库器 `Builder`(`builder.rs`)。
//!
//! 收集建库配置并产出 [`Mneme`](crate::memory::Mneme);字段语义见设计 16 §2。

use std::path::Path;
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
use crate::memory::table::Table;

/// 近似去重阈值的缺省值(统一按余弦口径)。
const DEFAULT_DEDUP_THRESHOLD: f32 = 0.95;

/// 访问统计落盘周期的缺省值(秒;仅记录,L5 生效)。
const DEFAULT_ACCESS_FLUSH_SECS: u64 = 30;
/// 建库器。
pub struct Builder {
    path: Option<std::path::PathBuf>,
    dimension: Option<u32>,
    metric: Metric,
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
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            path: None,
            dimension: None,
            metric: Metric::Cosine,
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
        }
    }
}

impl Builder {
    /// 设置存储目录;L1 尚未实现持久化,设置后 `build()` 返回 `Unsupported`。
    pub fn path(mut self, dir: impl AsRef<Path>) -> Self {
        self.path = Some(dir.as_ref().to_path_buf());
        self
    }

    /// 设置建库维度(新建必填)。
    pub fn dimension(mut self, dimension: u32) -> Self {
        self.dimension = Some(dimension);
        self
    }

    /// 设置距离度量(默认 [`Metric::Cosine`])。
    pub fn metric(mut self, metric: Metric) -> Self {
        self.metric = metric;
        self
    }

    /// 设置 fsync 策略(仅记录,L2 生效)。
    pub fn fsync(mut self, fsync: FsyncPolicy) -> Self {
        self.fsync = fsync;
        self
    }

    /// 设置同 key 写入行为。
    pub fn insert_mode(mut self, insert_mode: InsertMode) -> Self {
        self.insert_mode = insert_mode;
        self
    }

    /// 设置写入期去重策略。
    pub fn dedup(mut self, dedup: Dedup) -> Self {
        self.dedup = dedup;
        self
    }

    /// 设置近似去重阈值(默认 0.95,统一按余弦口径)。
    pub fn dedup_threshold(mut self, threshold: f32) -> Self {
        self.dedup_threshold = threshold;
        self
    }

    /// 设置量化格式(仅记录,L6 生效)。
    pub fn quantization(mut self, quantization: VectorFormat) -> Self {
        self.quantization = quantization;
        self
    }

    /// 设置 HNSW 参数(仅记录,L3 生效)。
    pub fn hnsw(mut self, hnsw: HnswParams) -> Self {
        self.hnsw = hnsw;
        self
    }

    /// 设置 compaction 策略(仅记录,L5 生效)。
    pub fn compaction(mut self, compaction: CompactionPolicy) -> Self {
        self.compaction = compaction;
        self
    }

    /// 开启/关闭后台自动遗忘(默认 `None` = 关闭)。
    pub fn retention(mut self, retention: Option<Retention>) -> Self {
        self.retention = retention;
        self
    }

    /// 设置后台遗忘扫描周期(仅记录,L5 生效)。
    pub fn retain_interval(mut self, interval: Duration) -> Self {
        self.retain_interval = Some(interval);
        self
    }

    /// 设置访问统计落盘周期(仅记录,L5 生效)。
    pub fn access_flush_interval(mut self, interval: Duration) -> Self {
        self.access_flush_interval = interval;
        self
    }

    /// 设置压缩策略(仅记录,L2 生效)。
    pub fn compression(mut self, compression: Compression) -> Self {
        self.compression = compression;
        self
    }

    /// 设置关系邻接索引方向。
    pub fn relation_index(mut self, relation_index: RelationIndex) -> Self {
        self.relation_index = relation_index;
        self
    }

    /// 设置并行度;`0` = 自动。
    pub fn parallelism(mut self, parallelism: usize) -> Self {
        self.parallelism = parallelism;
        self
    }

    /// 设置进阶调参。
    pub fn tuning(mut self, tuning: Tuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// 设置数据限额。
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// 注入时钟(测试确定性)。
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// 只读共享模式(仅记录,L12 生效)。
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// 打开时全量校验(仅记录,L2 生效)。
    pub fn verify_on_open(mut self, verify: bool) -> Self {
        self.verify_on_open = verify;
        self
    }

    /// 损坏段 fail-fast(仅记录,L2 生效)。
    pub fn fail_fast_on_corruption(mut self, fail_fast: bool) -> Self {
        self.fail_fast_on_corruption = fail_fast;
        self
    }

    /// 构建库句柄。
    ///
    /// # Errors
    /// * 未设置 `dimension` → [`MnemeError::Config`];
    /// * 设置了 `path` → [`MnemeError::Unsupported`](持久化在 L2 实现)。
    ///
    /// # Examples
    /// ```
    /// use mneme::Builder;
    /// let db = Builder::default().dimension(2).build().unwrap();
    /// assert!(db.list_namespaces().unwrap().is_empty());
    /// ```
    pub fn build(self) -> Result<Mneme> {
        if self.path.is_some() {
            return Err(MnemeError::Unsupported {
                feature: "持久化(path, L2)",
            });
        }
        let dimension = self.dimension.ok_or(MnemeError::Config {
            reason: "新建内存库必须指定维度",
        })?;
        let dimension = Dimension::new(dimension)?;
        let config = Config {
            dimension,
            metric: self.metric,
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
        };
        let config = Arc::new(config);
        Ok(Mneme {
            table: Arc::new(Table::new(Arc::clone(&config))),
            config,
            control: CompactionControl::new(),
        })
    }
}
