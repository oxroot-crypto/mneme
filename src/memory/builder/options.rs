//! 建库器 [`Builder`](super::Builder) 的链式配置 setter(`builder/options.rs`)。
//!
//! 仅承载参数写入,不包含校验逻辑(校验收敛在 [`Builder::build`](super::Builder::build));
//! 字段语义见设计 16 §2。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::core::metric::Metric;
use crate::core::options::{
    BuildPrecision, Clock, CompactionPolicy, Compression, FsyncPolicy, HnswParams, InsertMode,
    Limits, RelationIndex, Tuning, VectorFormat,
};
use crate::memory::dedup::Dedup;
use crate::memory::lifecycle::Retention;

use super::Builder;

impl Builder {
    /// 设置存储目录;设置后 `build()` 打开/新建持久库(设计 04 §1)。
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
        self.metric_explicit = true;
        self
    }

    /// 设置 fsync 策略(L2 生效)。
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
    /// * `threshold` - 余弦相似度阈值,`[0,1]`;越界或非有限值在 `build` 入口拒绝。
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

    /// 设置 HNSW 参数(L3 生效)。
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

    /// 是否启动后台维护线程(默认 `true`;`FC-LIFE-POST-010`)。
    ///
    /// `false` 时不自动 compaction、不自动遗忘、不周期落访问统计——适合批量导入/
    /// 建库期"先闸住维护、建完统一整理"的场景(避免维护与导入争抢 CPU/IO)。
    /// 手动 [`Mneme::maintenance_tick`](crate::memory::Mneme::maintenance_tick)/
    /// [`Mneme::compact`](crate::memory::Mneme::compact)/
    /// [`Namespace::retain`](crate::memory::Namespace::retain) 不受影响;只读实例的
    /// MANIFEST 探测线程与本开关无关。
    ///
    /// # Arguments
    ///
    /// * `enabled` - `true` 启动后台维护线程;`false` 不启动。
    ///
    /// # Returns
    ///
    /// 携带维护开关的构建器(链式)。
    ///
    /// # Examples
    /// ```
    /// use mneme::Builder;
    /// let db = Builder::default()
    ///     .dimension(2)
    ///     .maintenance(false)
    ///     .build()
    ///     .unwrap();
    /// let _ = db.namespace("demo");
    /// ```
    pub fn maintenance(mut self, enabled: bool) -> Self {
        self.maintenance = enabled;
        self
    }

    /// 设置 HNSW 建图距离精度档位(默认 [`BuildPrecision::Hybrid`])。
    ///
    /// 只影响 flush/compaction 的**新段**构建距离;不改变磁盘格式与查询语义。
    /// `Hybrid` 用段内临时 i8 码流近似遍历、选邻前 f32 精排;`F32` 为全精确原行为。
    ///
    /// # Arguments
    ///
    /// * `precision` - 建图精度档位。
    ///
    /// # Returns
    ///
    /// 携带建图精度的构建器(链式)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Builder, BuildPrecision};
    /// let db = Builder::default()
    ///     .dimension(2)
    ///     .build_precision(BuildPrecision::F32)
    ///     .build()
    ///     .unwrap();
    /// let _ = db.namespace("demo");
    /// ```
    pub fn build_precision(mut self, precision: BuildPrecision) -> Self {
        self.build_precision = precision;
        self
    }

    /// 设置 compaction 策略(L5 生效)。
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

    /// 设置后台遗忘扫描周期(L5 生效)。
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

    /// 设置访问统计落盘周期(L5 生效)。
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

    /// 设置压缩策略:写入时作用于 msec 记录体 `text`/`meta`/`provenance`
    /// (feature `compress` 提供自研 LZ4 风格 codec,`compress-zstd` 提供 zstd;
    /// 压缩无收益时回退原文)。未开对应 feature 时构造期返回 `Unsupported`
    /// (`FC-SEC-ERR-001`)。
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

    /// 设置只读实例探测新 MANIFEST 的周期(默认 1s;`Duration::ZERO` = 关闭)。
    ///
    /// # Arguments
    ///
    /// * `interval` - 探测周期;只读实例据此自动切换视图(I29)。
    ///
    /// # Returns
    ///
    /// 携带探测周期的构建器(链式)。
    pub fn read_only_probe_interval(mut self, interval: std::time::Duration) -> Self {
        self.read_only_probe_interval = interval;
        self
    }

    /// 设置事件可观测钩子(默认无;设计 12 §4)。
    ///
    /// # Arguments
    ///
    /// * `observer` - 事件回调;回调 panic 被隔离,不影响引擎行为(I30)。
    ///
    /// # Returns
    ///
    /// 携带观察者的构建器(链式)。
    pub fn observer(mut self, observer: std::sync::Arc<dyn crate::Observer>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// 设置自定义存储后端(默认 `FsStorage`;设计 12 §3.1)。
    ///
    /// # Arguments
    ///
    /// * `storage` - 根代理的 [`Storage`](crate::Storage) 实现(如 [`MemStorage`](crate::MemStorage))。
    ///
    /// # Returns
    ///
    /// 携带有存储后端的构建器(链式)。
    pub fn storage(mut self, storage: std::sync::Arc<dyn crate::Storage>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// 设置静态加密配置(`None` = 明文)。
    ///
    /// # Arguments
    ///
    /// * `encryption` - 密钥提供者与算法;开启后段/WAL/MANIFEST 写盘为 AEAD 信封。
    ///
    /// # Returns
    ///
    /// 携带加密配置的构建器(链式)。
    ///
    /// # Errors
    ///
    /// feature `encrypt` 未开启时在 `build()` 返回 `Unsupported`(绝不静默明文落盘)。
    pub fn encryption(mut self, encryption: Option<crate::crypto::Encryption>) -> Self {
        self.encryption = encryption;
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

    /// 只读共享模式(L2 已实现单进程只读打开:不持锁、不写盘)。
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

    /// 打开时全量校验(L2 生效)。
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

    /// 损坏段 fail-fast(L2 生效)。
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

    /// 注入 I/O 前置钩子(测试崩溃注入;设计 04 §10.1)。
    ///
    /// # Arguments
    ///
    /// * `hook` - 在每次 write/fsync/rename 前调用的回调;返回 `Err` 即注入故障。
    ///
    /// # Returns
    ///
    /// 携带钩子的构建器(链式)。
    ///
    /// # Examples
    /// ```
    /// use std::sync::Arc;
    /// use mneme::{Builder, FsyncHook, IoAction};
    ///
    /// struct Noop;
    /// impl FsyncHook for Noop {
    ///     fn before(&self, _action: IoAction<'_>) -> std::io::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let builder = Builder::default().fsync_hook(Arc::new(Noop));
    /// # let _ = builder;
    /// ```
    pub fn fsync_hook(mut self, hook: Arc<dyn crate::FsyncHook>) -> Self {
        self.fsync_hook = Some(hook);
        self
    }
}
