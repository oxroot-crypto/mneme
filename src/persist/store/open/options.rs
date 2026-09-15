//! 打开参数 [`OpenOptions`]。

use std::sync::Arc;

use crate::core::metric::Metric;
use crate::core::options::{Compression, FsyncPolicy, Limits, Tuning};
use crate::memory::index::IndexFactory;
use crate::persist::hook::FsyncHook;

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
    /// 数据限额(帧上限等写入路径防线;设计 16 §8)。
    pub(crate) limits: Limits,
}
