//! 建库器 [`Builder`] 的类型定义、缺省值与调试输出。

use std::sync::Arc;
use std::time::Duration;

use crate::core::metric::Metric;
use crate::core::options::{
    BuildPrecision, Clock, CompactionPolicy, Compression, Dimension, FsyncPolicy, HnswParams,
    InsertMode, Limits, RelationIndex, SystemClock, Tuning, VectorFormat,
};
use crate::memory::dedup::Dedup;
use crate::memory::lifecycle::Retention;
use crate::memory::table::WriterState;
use crate::persist::hook::FsyncHook;
use crate::persist::store::Store;

/// [`Builder::open_backend`] 的返回:持久后端 + 初始写状态 + 维度 + 度量。
pub(super) type OpenedBackend = (Option<Arc<Store>>, Option<WriterState>, Dimension, Metric);

/// 近似去重阈值的缺省值(统一按余弦口径)。
pub(super) const DEFAULT_DEDUP_THRESHOLD: f32 = 0.95;

/// 访问统计落盘周期的缺省值(秒;仅记录,L5 生效)。
pub(super) const DEFAULT_ACCESS_FLUSH_SECS: u64 = 30;

/// 建库器。
pub struct Builder {
    pub(super) path: Option<std::path::PathBuf>,
    pub(super) dimension: Option<u32>,
    pub(super) metric: Metric,
    pub(super) metric_explicit: bool,
    pub(super) fsync: FsyncPolicy,
    pub(super) insert_mode: InsertMode,
    pub(super) dedup: Dedup,
    pub(super) dedup_threshold: f32,
    pub(super) quantization: VectorFormat,
    pub(super) hnsw: HnswParams,
    pub(super) build_precision: BuildPrecision,
    pub(super) compaction: CompactionPolicy,
    pub(super) retention: Option<Retention>,
    pub(super) retain_interval: Option<Duration>,
    pub(super) access_flush_interval: Duration,
    pub(super) compression: Compression,
    pub(super) encryption: Option<crate::crypto::Encryption>,
    pub(super) observer: Option<Arc<dyn crate::core::observe::Observer>>,
    pub(super) read_only_probe_interval: std::time::Duration,
    pub(super) storage: Option<Arc<dyn crate::persist::storage::Storage>>,
    pub(super) relation_index: RelationIndex,
    pub(super) parallelism: usize,
    pub(super) maintenance: bool,
    pub(super) tuning: Tuning,
    pub(super) limits: Limits,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) read_only: bool,
    pub(super) verify_on_open: bool,
    pub(super) fail_fast_on_corruption: bool,
    pub(super) fsync_hook: Option<Arc<dyn FsyncHook>>,
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
