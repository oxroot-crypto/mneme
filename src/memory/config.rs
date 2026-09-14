//! L1 运行配置(`Builder` 的产物)。
//!
//! 这些字段在 L1 内存层大多仅作记录;真正生效的语义随对应层落地
//! (如 `hnsw`/`quantization` 在 L3/L6,`compaction`/`retention` 在 L5)。

use std::sync::Arc;
use std::time::Duration;

use crate::core::metric::Metric;
use crate::core::options::{
    BuildPrecision, Clock, CompactionPolicy, Compression, Dimension, HnswParams, InsertMode,
    Limits, RelationIndex, Tuning, VectorFormat,
};
use crate::memory::dedup::Dedup;
use crate::memory::index::IndexFactory;
use crate::memory::lifecycle::Retention;

/// 建库配置;由 [`Builder`](crate::memory::Builder) 构造,建库后不可变。
///
pub(crate) struct Config {
    pub(crate) dimension: Dimension,
    pub(crate) metric: Metric,
    pub(crate) insert_mode: InsertMode,
    pub(crate) dedup: Dedup,
    pub(crate) dedup_threshold: f32,
    pub(crate) quantization: VectorFormat,
    pub(crate) hnsw: HnswParams,
    /// HNSW 建图距离精度档位(设计 05 §4.4;`FC-INDEX-POST-010/011`)。
    pub(crate) build_precision: BuildPrecision,
    /// 向量索引工厂(L3);`None` = 纯暴力(L1 语义)。
    pub(crate) index_factory: Option<Arc<dyn IndexFactory>>,
    pub(crate) compaction: CompactionPolicy,
    pub(crate) retention: Option<Retention>,
    pub(crate) retain_interval: Option<Duration>,
    pub(crate) access_flush_interval: Duration,
    pub(crate) compression: Compression,
    /// 事件可观测钩子(默认 `None`,零成本;设计 12 §4)。
    pub(crate) observer: Option<Arc<dyn crate::core::observe::Observer>>,
    pub(crate) relation_index: RelationIndex,
    pub(crate) parallelism: usize,
    pub(crate) tuning: Tuning,
    pub(crate) limits: Limits,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) read_only: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("dimension", &self.dimension)
            .field("metric", &self.metric)
            .field("insert_mode", &self.insert_mode)
            .field("relation_index", &self.relation_index)
            .finish_non_exhaustive()
    }
}
