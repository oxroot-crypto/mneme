//! 库身份、存储后端与打开可靠性 setter。

use std::path::Path;
use std::sync::Arc;

use crate::core::metric::Metric;
use crate::core::options::{Compression, FsyncPolicy};

use super::super::Builder;

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
