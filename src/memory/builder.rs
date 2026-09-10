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
    ///
    /// # Arguments
    ///
    /// * `dir` - 存储目录路径。
    ///
    /// # Returns
    ///
    /// 携带存储目录的构建器(链式)。
    pub fn path(mut self, dir: impl AsRef<Path>) -> Self {
        self.path = Some(dir.as_ref().to_path_buf());
        self
    }

    /// 设置建库维度(新建必填)。
    ///
    /// # Arguments
    ///
    /// * `dimension` - 向量维度,必须落在 `[1, 65536]`。
    ///
    /// # Returns
    ///
    /// 携带维度的构建器(链式)。
    pub fn dimension(mut self, dimension: u32) -> Self {
        self.dimension = Some(dimension);
        self
    }

    /// 设置距离度量(默认 [`Metric::Cosine`])。
    ///
    /// # Arguments
    ///
    /// * `metric` - 三种距离度量之一。
    ///
    /// # Returns
    ///
    /// 携带度量的构建器(链式)。
    pub fn metric(mut self, metric: Metric) -> Self {
        self.metric = metric;
        self
    }

    /// 设置 fsync 策略(仅记录,L2 生效)。
    ///
    /// # Arguments
    ///
    /// * `fsync` - 落盘同步策略。
    ///
    /// # Returns
    ///
    /// 携带 fsync 策略的构建器(链式)。
    pub fn fsync(mut self, fsync: FsyncPolicy) -> Self {
        self.fsync = fsync;
        self
    }

    /// 设置同 key 写入行为。
    ///
    /// # Arguments
    ///
    /// * `insert_mode` - `Upsert`(覆盖)或 `RejectDuplicate`(拒绝重复)。
    ///
    /// # Returns
    ///
    /// 携带写入行为的构建器(链式)。
    pub fn insert_mode(mut self, insert_mode: InsertMode) -> Self {
        self.insert_mode = insert_mode;
        self
    }

    /// 设置写入期去重策略。
    ///
    /// # Arguments
    ///
    /// * `dedup` - 去重策略(可携带 `Merge` 回调)。
    ///
    /// # Returns
    ///
    /// 携带去重策略的构建器(链式)。
    pub fn dedup(mut self, dedup: Dedup) -> Self {
        self.dedup = dedup;
        self
    }

    /// 设置近似去重阈值(默认 0.95,统一按余弦口径)。
    ///
    /// # Arguments
    ///
    /// * `threshold` - 余弦相似度阈值,`[0,1]`。
    ///
    /// # Returns
    ///
    /// 携带阈值的构建器(链式)。
    pub fn dedup_threshold(mut self, threshold: f32) -> Self {
        self.dedup_threshold = threshold;
        self
    }

    /// 设置量化格式(仅记录,L6 生效)。
    ///
    /// # Arguments
    ///
    /// * `quantization` - 向量量化格式。
    ///
    /// # Returns
    ///
    /// 携带量化格式的构建器(链式)。
    pub fn quantization(mut self, quantization: VectorFormat) -> Self {
        self.quantization = quantization;
        self
    }

    /// 设置 HNSW 参数(仅记录,L3 生效)。
    ///
    /// # Arguments
    ///
    /// * `hnsw` - 图参数。
    ///
    /// # Returns
    ///
    /// 携带 HNSW 参数的构建器(链式)。
    pub fn hnsw(mut self, hnsw: HnswParams) -> Self {
        self.hnsw = hnsw;
        self
    }

    /// 设置 compaction 策略(仅记录,L5 生效)。
    ///
    /// # Arguments
    ///
    /// * `compaction` - 后台合并策略。
    ///
    /// # Returns
    ///
    /// 携带 compaction 策略的构建器(链式)。
    pub fn compaction(mut self, compaction: CompactionPolicy) -> Self {
        self.compaction = compaction;
        self
    }

    /// 开启/关闭后台自动遗忘(默认 `None` = 关闭)。
    ///
    /// # Arguments
    ///
    /// * `retention` - 遗忘策略;`None` = 关闭后台自动遗忘。
    ///
    /// # Returns
    ///
    /// 携带遗忘策略的构建器(链式)。
    pub fn retention(mut self, retention: Option<Retention>) -> Self {
        self.retention = retention;
        self
    }

    /// 设置后台遗忘扫描周期(仅记录,L5 生效)。
    ///
    /// # Arguments
    ///
    /// * `interval` - 扫描周期。
    ///
    /// # Returns
    ///
    /// 携带扫描周期的构建器(链式)。
    pub fn retain_interval(mut self, interval: Duration) -> Self {
        self.retain_interval = Some(interval);
        self
    }

    /// 设置访问统计落盘周期(仅记录,L5 生效)。
    ///
    /// # Arguments
    ///
    /// * `interval` - 落盘周期。
    ///
    /// # Returns
    ///
    /// 携带落盘周期的构建器(链式)。
    pub fn access_flush_interval(mut self, interval: Duration) -> Self {
        self.access_flush_interval = interval;
        self
    }

    /// 设置压缩策略(仅记录,L2 生效)。
    ///
    /// # Arguments
    ///
    /// * `compression` - 文本/元数据压缩策略。
    ///
    /// # Returns
    ///
    /// 携带压缩策略的构建器(链式)。
    pub fn compression(mut self, compression: Compression) -> Self {
        self.compression = compression;
        self
    }

    /// 设置关系邻接索引方向。
    ///
    /// # Arguments
    ///
    /// * `relation_index` - `Outgoing`(仅出边)或 `Both`(出边 + 反向)。
    ///
    /// # Returns
    ///
    /// 携带索引方向的构建器(链式)。
    pub fn relation_index(mut self, relation_index: RelationIndex) -> Self {
        self.relation_index = relation_index;
        self
    }

    /// 设置并行度;`0` = 自动。
    ///
    /// # Arguments
    ///
    /// * `parallelism` - 并行扫描线程数;`0` 表示自动探测。
    ///
    /// # Returns
    ///
    /// 携带并行度的构建器(链式)。
    pub fn parallelism(mut self, parallelism: usize) -> Self {
        self.parallelism = parallelism;
        self
    }

    /// 设置进阶调参。
    ///
    /// # Arguments
    ///
    /// * `tuning` - 暴力扫描分块/字典上限/布隆参数等进阶项。
    ///
    /// # Returns
    ///
    /// 携带调参的构建器(链式)。
    pub fn tuning(mut self, tuning: Tuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// 设置数据限额。
    ///
    /// # Arguments
    ///
    /// * `limits` - key/text/metadata 等限额。
    ///
    /// # Returns
    ///
    /// 携带限额的构建器(链式)。
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// 注入时钟(测试确定性)。
    ///
    /// # Arguments
    ///
    /// * `clock` - 时间源;测试可注入可回拨/快进的假时钟。
    ///
    /// # Returns
    ///
    /// 携带时钟的构建器(链式)。
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// 只读共享模式(仅记录,L12 生效)。
    ///
    /// # Arguments
    ///
    /// * `read_only` - `true` = 只读打开,任何写操作被拒。
    ///
    /// # Returns
    ///
    /// 携带只读标记的构建器(链式)。
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// 打开时全量校验(仅记录,L2 生效)。
    ///
    /// # Arguments
    ///
    /// * `verify` - `true` = 打开即校验各段 payload CRC(慢)。
    ///
    /// # Returns
    ///
    /// 携带校验开关的构建器(链式)。
    pub fn verify_on_open(mut self, verify: bool) -> Self {
        self.verify_on_open = verify;
        self
    }

    /// 损坏段 fail-fast(仅记录,L2 生效)。
    ///
    /// # Arguments
    ///
    /// * `fail_fast` - `true` = 遇损坏段直接拒绝启动,而非隔离剔除。
    ///
    /// # Returns
    ///
    /// 携带 fail-fast 开关的构建器(链式)。
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
