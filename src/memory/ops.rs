//! 运维报告与运行统计类型(`ops.rs`)。
//!
//! 这些类型的字段是稳定契约(设计 16 §1.6);L1 内存层只填充内存可得的子集,
//! 段/WAL/量化/压缩等字段随 L2–L6 落地。

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::core::options::{Compression, VectorFormat};
use crate::core::types::SegmentId;

/// 延迟直方图的桶数。
const HISTOGRAM_BUCKETS: usize = 32;

/// 延迟直方图下界(毫秒)。
const HISTOGRAM_MIN_MS: f64 = 1.0;

/// 延迟直方图上界(毫秒)。
const HISTOGRAM_MAX_MS: f64 = 1000.0;

/// 延迟直方图最高桶下标。
const HISTOGRAM_MAX_INDEX: usize = HISTOGRAM_BUCKETS - 1;

/// 单个段的统计。
///
/// 标记 `#[non_exhaustive]`:字段随层落地会继续扩展(如 L5 compaction 统计),
/// 下游不得依赖穷尽构造或穷尽匹配(设计 16 §1.6)。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SegmentStat {
    /// 段编号。
    pub id: SegmentId,
    /// 行数(含墓碑与历史版本)。
    pub rows: u64,
    /// 段文件字节数(vsec + 已落盘 hidx;不含 msec,口径见设计 16 §1.6)。
    pub bytes: u64,
    /// 墓碑/过期占比。
    pub dead_ratio: f32,
    /// 创建时刻(Unix 毫秒)。
    pub created: i64,
    /// HNSW 图节点数(无索引段为 0;取自真实载入的索引)。
    pub index_nodes: u64,
    /// HNSW 图最高层级(无索引段为 0)。
    pub index_levels: u8,
    /// 该段实际生效的量化格式(`F32` = 无副本或建段抽样已回退)。
    pub quant: VectorFormat,
    /// 建段抽样召回一致率估计(重开后为 `None`,仅原建段会话可见)。
    pub recall_est: Option<f32>,
}

/// 单命名空间统计。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NsStat {
    /// 文档数。
    pub doc_count: u64,
    /// 文本总长度。
    pub total_doc_len: u64,
}

/// 固定 32 桶延迟直方图(对数刻度,1ms..1s)。
#[derive(Debug, Clone, Default)]
pub struct Histogram {
    buckets: [u64; HISTOGRAM_BUCKETS],
}

impl Histogram {
    /// 记录一次延迟样本(毫秒),超范围钳制到端点桶。
    ///
    /// # Arguments
    /// * `latency_ms` - 延迟样本(毫秒);≤ 1ms 记入首桶,≥ 1s 记入末桶,其余按对数刻度分桶。
    ///
    /// # Examples
    /// ```
    /// use mneme::Histogram;
    /// let mut histogram = Histogram::default();
    /// histogram.record(12.0);
    /// assert_eq!(histogram.buckets().iter().sum::<u64>(), 1);
    /// ```
    pub fn record(&mut self, latency_ms: f64) {
        let index = if latency_ms <= HISTOGRAM_MIN_MS {
            0
        } else if latency_ms >= HISTOGRAM_MAX_MS {
            HISTOGRAM_MAX_INDEX
        } else {
            let log = latency_ms.log2() / HISTOGRAM_MAX_MS.log2();
            ((log * HISTOGRAM_MAX_INDEX as f64).round() as usize).min(HISTOGRAM_MAX_INDEX)
        };
        self.buckets[index] += 1;
    }

    /// 返回 32 个桶的累计计数。
    ///
    /// # Returns
    ///
    /// 长度恒 32 的桶计数数组:下标 0 为 ≤1ms、下标 31 为 ≥1s,其余按对数刻度
    /// 分布;计数由 [`record`](Self::record) 累加,读取不清零。
    pub fn buckets(&self) -> &[u64; HISTOGRAM_BUCKETS] {
        &self.buckets
    }
}

/// 量化运行状态(L6)。
#[derive(Debug, Clone, PartialEq)]
pub struct QuantStat {
    /// 用户配置的格式。
    pub configured: VectorFormat,
    /// 实际生效格式(可能因 I13 自动回退)。
    pub active: VectorFormat,
    /// 抽样查询的粗/精排名一致率估计。
    pub recall_est: Option<f32>,
}

/// 存储安全配置与迁移统计(加密/压缩均已落地并接线;分别受 feature
/// `encrypt` 与 `compress`/`compress-zstd` 门控)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageStat {
    /// 是否启用静态加密:库以加密配置打开时为 `true`(需 feature `encrypt`;
    /// 纯内存库恒 `false`)。
    pub encryption: bool,
    /// 文本/元数据压缩的配置值(已作用于 msec 记录体与 WAL 负载的字节编码;
    /// `Lz4` 需 feature `compress`,`Zstd` 需 feature `compress-zstd`)。
    pub compression: Compression,
    /// 已迁移段数。
    pub migrated_segments: usize,
    /// 总段数。
    pub total_segments: usize,
}

/// 版本链/历史保留统计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryStat {
    /// 当前保留的物理版本总数(含历史版本与墓碑)。
    pub retained_versions: u64,
    /// 累计已回收的版本数(仅 `history_horizon` 有限时增长)。
    pub reclaimed_versions: u64,
    /// 当前生效的历史保留窗口,`None` = 永久。
    pub horizon: Option<Duration>,
}

/// 后台合并状态。
#[derive(Debug, Clone, PartialEq, Default)]
pub enum CompactionState {
    /// 空闲。
    #[default]
    Idle,
    /// 运行中。
    Running {
        /// 进度,`[0,1]`。
        progress: f32,
        /// 参与合并的段。
        segments: Vec<SegmentId>,
    },
    /// 运行中被 `pause()` 挂起;`resume()` 后回到 `Running`(FC-LIFE-STA-001)。
    Paused {
        /// 暂停时已完成的进度,`[0,1]`。
        progress: f32,
        /// 参与合并的段。
        segments: Vec<SegmentId>,
    },
}

/// `db.stats()` 的运行统计(字段为稳定契约)。
#[derive(Debug, Clone)]
pub struct Stats {
    /// 段统计。
    pub segments: Vec<SegmentStat>,
    /// WAL 字节数。
    pub wal_bytes: u64,
    /// 进程内存估算。
    pub memory_est: u64,
    /// `trash/` 字节数。
    pub trash_bytes: u64,
    /// 查询延迟直方图。
    pub query_latency: Histogram,
    /// 每命名空间统计。
    pub per_namespace: HashMap<String, NsStat>,
    /// 量化状态。
    pub quant: QuantStat,
    /// 后台合并状态。
    pub compaction: CompactionState,
    /// 最近一次后台遗忘(未开启则 `None`)。
    pub retain: Option<crate::memory::lifecycle::RetainReport>,
    /// 关系边数。
    pub relations: u64,
    /// 版本链/历史保留统计。
    pub history: HistoryStat,
    /// 存储安全配置与迁移统计(加密/压缩均已落地,字段语义见 [`StorageStat`])。
    pub storage: StorageStat,
}

/// `check()` 的 fsck 报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckReport {
    /// 是否全绿。
    pub ok: bool,
    /// 损坏段列表。
    pub corrupted: Vec<SegmentId>,
    /// 修复建议。
    pub suggestions: Vec<String>,
}

/// `backup_to()` 的报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupReport {
    /// 备份文件数。
    pub files: usize,
    /// 备份字节数。
    pub bytes: u64,
    /// 是否走了硬链接路径。
    pub hardlinked: bool,
}

/// `SnapshotHandle::stats()` 的只读视图统计(设计 07 §6)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotStats {
    /// 视图基线序号水位(与 `SnapshotHandle::version` 一致)。
    pub version: u64,
    /// 视图内活跃段数(`slot_segment` 去重)。
    pub segments: usize,
    /// 视图内物理槽位数(含历史版本与墓碑)。
    pub rows: u64,
}

/// 后台合并控制句柄(见设计 16 §1.6);克隆共享同一状态。
#[derive(Debug, Clone, Default)]
pub struct CompactionControl {
    inner: std::sync::Arc<CompactionInner>,
}

#[derive(Debug, Default)]
struct CompactionInner {
    paused: AtomicBool,
    state: Mutex<CompactionState>,
}

impl CompactionControl {
    /// 新建控制句柄。
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 暂停后台合并。
    ///
    /// 运行中调用时状态转 `Paused`;空闲时调用只置暂停标志,状态保持 `Idle`
    /// (非法转移不改变状态,FC-LIFE-STA-001)。
    ///
    /// # Examples
    /// ```
    /// use mneme::CompactionControl;
    /// let control = CompactionControl::default();
    /// control.pause();
    /// assert!(control.is_paused());
    /// control.resume();
    /// assert!(!control.is_paused());
    /// ```
    pub fn pause(&self) {
        self.inner.paused.store(true, Ordering::Relaxed);
        let mut guard = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let CompactionState::Running { progress, segments } = &*guard {
            *guard = CompactionState::Paused {
                progress: *progress,
                segments: segments.clone(),
            };
        }
    }

    /// 恢复后台合并;`Paused` 时状态转回 `Running`,其余状态不变。
    pub fn resume(&self) {
        self.inner.paused.store(false, Ordering::Relaxed);
        let mut guard = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let CompactionState::Paused { progress, segments } = &*guard {
            *guard = CompactionState::Running {
                progress: *progress,
                segments: segments.clone(),
            };
        }
    }

    /// 是否处于暂停状态。
    ///
    /// # Returns
    ///
    /// 暂停标志已置位返回 `true`;空闲时调用 `pause()` 也会置位,此时
    /// [`state`](Self::state) 仍为 [`Idle`](CompactionState::Idle)。
    pub fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::Relaxed)
    }

    /// 当前合并状态。
    ///
    /// # Returns
    ///
    /// 当前 [`CompactionState`] 的克隆:`Idle`、`Running`(含进度与参与段)或
    /// `Paused`(含暂停时进度与参与段)。
    pub fn state(&self) -> CompactionState {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 更新内部状态(供后台任务调用;L5 起使用)。
    pub(crate) fn set_state(&self, state: CompactionState) {
        *self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = state;
    }

    /// 标记合并开始(参与段列表)。
    pub(crate) fn mark_running(&self, segments: Vec<SegmentId>) {
        self.set_state(CompactionState::Running {
            progress: 0.0,
            segments,
        });
    }

    /// 更新合并进度(`[0,1]`)。
    pub(crate) fn mark_progress(&self, progress: f32) {
        let mut guard = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let CompactionState::Running { segments, .. } = &*guard {
            let segments = segments.clone();
            *guard = CompactionState::Running {
                progress: progress.clamp(0.0, 1.0),
                segments,
            };
        }
    }

    /// 标记合并回到空闲。
    pub(crate) fn mark_idle(&self) {
        self.set_state(CompactionState::Idle);
    }
}

/// 一次 compaction 的选段计划(内部;由 L5 调度器产出,持久层执行)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionPlan {
    /// 参与本轮合并的段编号(按 MANIFEST 顺序)。
    pub(crate) segments: Vec<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FC-LIFE-STA-001:`pause` 在 `Running` 上转 `Paused`、`resume` 转回;
    /// 非法转移(`Idle` 上 `Pause`/`Resume`)不改变状态。
    #[test]
    fn compaction_state_transitions_follow_spec() {
        let control = CompactionControl::new();
        assert_eq!(control.state(), CompactionState::Idle);
        // Idle 上 pause:非法转移,状态保持 Idle,只置标志。
        control.pause();
        assert_eq!(control.state(), CompactionState::Idle);
        assert!(control.is_paused());
        control.resume();
        assert_eq!(control.state(), CompactionState::Idle);

        control.mark_running(vec![SegmentId::new(3), SegmentId::new(4)]);
        control.mark_progress(0.5);
        control.pause();
        assert_eq!(
            control.state(),
            CompactionState::Paused {
                progress: 0.5,
                segments: vec![SegmentId::new(3), SegmentId::new(4)],
            }
        );
        control.resume();
        assert!(matches!(control.state(), CompactionState::Running { .. }));
        control.mark_idle();
        assert_eq!(control.state(), CompactionState::Idle);
    }
}
