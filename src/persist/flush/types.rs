//! 段物化个共享类型:输入、产物同子模块之间个中间态。
//!
//! 这些类型横跨 `encode`、`quant`、`index` 三只子模块,集中到箇里免得相互引用。

use std::sync::Arc;

use crate::core::options::VectorFormat;
use crate::memory::index::{QuantCopy, VectorIndex};
use crate::persist::msec::{DeltaEntry, SlotMeta};

/// 一次段物化的输入:要写入的全局槽位(升序)与跨段 delta。
pub(crate) struct SegmentBuildInput<'a> {
    /// 要物化的全局槽位下标(升序);空 = 纯 delta 段。
    pub(crate) slots: &'a [usize],
    /// 跨段覆盖条目(访问/关系);`full_relations` 为真时忽略关系部分。
    pub(crate) delta: &'a [DeltaEntry],
    /// 是否全量重写关系表(首个段或 compaction);否则关系变更走 delta。
    pub(crate) full_relations: bool,
    /// 本段 HNSW 批内建图并行度(`0` = 可用核数)。
    ///
    /// 多块 flush 时块间已并行,内层应传 `1` 避免嵌套过度订阅;
    /// 单块/compaction 逐段构建时传库配置并行度。
    pub(crate) parallelism: usize,
}

/// 段内槽位及其对应的向量/范数/删除位(向量借用自写状态)。
pub(super) struct SegmentSlots<'a> {
    pub(super) slots: Vec<SlotMeta>,
    pub(super) vectors: Vec<&'a [f32]>,
    pub(super) norms: Vec<f32>,
    pub(super) dead: Vec<bool>,
}

/// 一次段编码的产物:vsec/msec/hidx 字节与内存索引(供 flush 安装到写状态)。
pub(crate) struct EncodedSegment {
    /// 向量段字节。
    pub(crate) vsec: Vec<u8>,
    /// 元数据段字节。
    pub(crate) msec: Vec<u8>,
    /// HNSW 图段字节(节点为空或未配置索引工厂时为 `None`)。
    pub(crate) hidx: Option<Vec<u8>>,
    /// 内存索引(供 flush 安装到写状态;与 `hidx` 对应)。
    pub(crate) index: Option<Arc<dyn VectorIndex>>,
    /// 段内入口槽位(无索引时为 0)。
    pub(crate) entry_slot: u32,
    /// 段内入口层级(无索引时为 0)。
    pub(crate) entry_level: u8,
    /// 段实际生效的量化格式(`F32` = 无副本或建段抽样已回退)。
    pub(crate) quant: VectorFormat,
    /// 建段抽样召回一致率估计(`None` = 无副本)。
    pub(crate) recall_est: Option<f32>,
}

/// 量化建段决策:保留副本 / 回退 / 召回估计。
pub(super) struct QuantDecision {
    /// 保留的副本(`None` = 无需量化或抽样不达标已回退)。
    pub(super) copy: Option<QuantCopy>,
    /// 抽样一致率估计(仅保留副本时非空)。
    pub(super) recall_est: Option<f32>,
    /// 是否因抽样不达标回退(调用方需重建无副本索引)。
    pub(super) fallback: bool,
}
