//! L0 全局配置与选项类型(纯数据定义,不含行为)。
//!
//! 这些类型贯穿全库:L0 只给出数据形状与默认值,具体语义由对应层实现
//! (如 `HnswParams` 在 L3、`CompactionPolicy` 在 L5、`Scoring` 在 L4)。
//! 行为型成员(如 `RelationKind::custom` 的名称注册表、`Encryption` 的密钥提供者)
//! 依赖上层状态与 feature,留待对应层补充。

use std::time::Duration;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::types::RowId;

/// 向量维度。
///
/// 取值闭区间 `[1, 65536]`;构造时校验,内部不再使用裸整数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dimension(u32);

impl Dimension {
    /// 允许的最小维度。
    pub const MIN: u32 = 1;
    /// 允许的最大维度。
    pub const MAX: u32 = 65_536;

    /// 校验并构造一个维度。
    ///
    /// # Arguments
    ///
    /// * `value` - 维度,必须落在 `[1, 65536]`。
    ///
    /// # Errors
    ///
    /// 越界时返回 [`MnemeError::Invalid`]。
    ///
    /// # Examples
    ///
    /// ```
    /// use mneme::Dimension;
    ///
    /// assert_eq!(Dimension::new(1536).unwrap().get(), 1536);
    /// assert!(Dimension::new(0).is_err());
    /// assert!(Dimension::new(65_537).is_err());
    /// ```
    pub fn new(value: u32) -> Result<Self> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(MnemeError::Invalid("维度必须在 1..=65536 之间"))
        }
    }

    /// 返回内部维度值。
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// 落盘同步(fsync)策略。
///
/// 语义与权衡见设计 00 §6.1 与 04 §3。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsyncPolicy {
    /// 每次写入后立即 fsync。
    Always,
    /// 按时间窗口批量 fsync。
    Batched(Duration),
    /// 仅在显式 `flush()` 时 fsync。
    OnFlush,
    /// 从不 fsync(**仅供测试**)。
    Never,
}

impl Default for FsyncPolicy {
    fn default() -> Self {
        Self::Batched(Duration::from_millis(20))
    }
}

/// 同一 key 的写入行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InsertMode {
    /// 覆盖既有记录(默认)。
    #[default]
    Upsert,
    /// 重复 key 返回 [`MnemeError::DuplicateKey`]。
    RejectDuplicate,
}

/// 向量量化格式(量化实现见 L6)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorFormat {
    /// 仅 f32 原向量(默认)。
    #[default]
    F32,
    /// 额外维护 f16 量化副本(feature `quant-f16`)。
    F16,
    /// 额外维护 i8 量化副本 + 两阶段重打分。
    I8Rescored,
}

/// HNSW 图参数(L3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswParams {
    /// 上层度数上限,默认 16。
    pub m: u16,
    /// 第 0 层度数上限,默认 32。
    pub m0: u16,
    /// 构建期探查宽度,默认 200。
    pub ef_construction: u16,
    /// 查询期默认探查宽度,默认 64。
    pub ef_search: u16,
}

impl Default for HnswParams {
    fn default() -> Self {
        Self {
            m: 16,
            m0: 32,
            ef_construction: 200,
            ef_search: 64,
        }
    }
}

/// 后台 compaction 策略(L5)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactionPolicy {
    /// 分级比 r,默认 4。
    pub tier_ratio: u32,
    /// 同层合并阈值 T,默认 4。
    pub tier_count: u32,
    /// 墓碑 + 过期占比触发线,默认 0.25。
    pub dead_ratio: f32,
    /// WAL 压力触发线(字节),默认 256 MiB。
    pub wal_bytes: u64,
    /// 单个 WAL 文件轮转阈值(字节),默认 64 MiB。
    pub wal_file_bytes: u64,
    /// 段初始目标行数 B,默认 8192。
    pub segment_rows: u64,
    /// 后台合并磁盘配额,默认 0.30。
    pub io_budget: f32,
    /// 历史版本保留窗口;`None` 表示永久保留(默认)。
    pub history_horizon: Option<Duration>,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            tier_ratio: 4,
            tier_count: 4,
            dead_ratio: 0.25,
            wal_bytes: 256 * 1024 * 1024,
            wal_file_bytes: 64 * 1024 * 1024,
            segment_rows: 8192,
            io_budget: 0.30,
            history_horizon: None,
        }
    }
}

/// 进阶调参(通常保持默认)。
#[derive(Debug, Clone, PartialEq)]
pub struct Tuning {
    /// 暴力扫描的计算分块粒度,默认 8192。
    pub parallel_block: usize,
    /// 每段可索引字段上限,默认 16。
    pub field_dict_max: u16,
    /// 布隆过滤器目标误判率,默认 0.01。
    pub bloom_fpp: f32,
    /// 段行数低于此值恒用暴力扫描,默认 2048。
    pub brute_force_max_rows: u32,
    /// 过滤三档:后过滤 / 约束遍历分界,默认 0.10。
    pub filter_post_threshold: f32,
    /// 过滤三档:约束遍历 / 候选暴力分界,默认 0.001。
    pub filter_brute_threshold: f32,
    /// 是否启用内置停用词表,默认 `true`。
    pub stopwords: bool,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            parallel_block: 8192,
            field_dict_max: 16,
            bloom_fpp: 0.01,
            brute_force_max_rows: 2048,
            filter_post_threshold: 0.10,
            filter_brute_threshold: 0.001,
            stopwords: true,
        }
    }
}

/// 数据限额(见设计 16 §8)。超限一律返回 `TooLarge` 或 `Invalid`,绝不静默截断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// key 最大字节数(UTF-8),默认 1024。
    pub key_bytes: usize,
    /// text 最大字节数,默认 1 MiB。
    pub text_bytes: usize,
    /// metadata JSON 最大字节数,默认 64 KiB。
    pub meta_bytes: usize,
    /// metadata 最大嵌套深度,默认 32。
    pub meta_depth: u16,
    /// 命名空间最大深度,默认 32。
    pub ns_depth: u16,
    /// `top_k` 上限,默认 4096。
    pub top_k_max: u32,
    /// `ef` 上限,默认 4096。
    pub ef_max: u32,
    /// WAL 单帧 payload 上限(字节),默认 16 MiB。
    pub wal_frame_max: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            key_bytes: 1024,
            text_bytes: 1024 * 1024,
            meta_bytes: 64 * 1024,
            meta_depth: 32,
            ns_depth: 32,
            top_k_max: 4096,
            ef_max: 4096,
            wal_frame_max: 16 * 1024 * 1024,
        }
    }
}

/// 记忆感知排序的综合打分配置(L4)。
///
/// 各权重为 0 即关闭该因子;默认仅相似度权重为 1.0。
#[derive(Debug, Clone, PartialEq)]
pub struct Scoring {
    /// 相似度权重,默认 1.0。
    pub w_sim: f32,
    /// 时间新鲜度权重,默认 0.0(关闭)。
    pub w_recency: f32,
    /// 重要度权重,默认 0.0。
    pub w_importance: f32,
    /// 访问频次权重,默认 0.0。
    pub w_access: f32,
    /// 可信度权重,默认 0.0。
    pub w_confidence: f32,
    /// 新鲜度半衰期,默认 14 天。
    pub half_life: Duration,
    /// 访问次数归一基准,默认 100。
    pub c_norm: u32,
    /// 相似度保底,默认 0.0。
    pub floor: f32,
    /// 新鲜度时间轴,默认 [`TimeAxis::ValidTime`]。
    pub time_axis: TimeAxis,
    /// HNSW 遍历是否按重要性偏置(只改访问顺序),默认 `false`。
    pub bias_routing: bool,
}

impl Scoring {
    /// 返回默认配置(等价于 [`Scoring::default`])。
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for Scoring {
    fn default() -> Self {
        Self {
            w_sim: 1.0,
            w_recency: 0.0,
            w_importance: 0.0,
            w_access: 0.0,
            w_confidence: 0.0,
            half_life: Duration::from_secs(14 * 24 * 60 * 60),
            c_norm: 100,
            floor: 0.0,
            time_axis: TimeAxis::default(),
            bias_routing: false,
        }
    }
}

/// 新鲜度打分使用的时间轴(L4)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeAxis {
    /// 有效时间(默认)。
    #[default]
    ValidTime,
    /// 事务时间。
    TransactionTime,
}

/// 结果多样性策略(L4)。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Diversity {
    /// 关闭多样性(默认)。
    #[default]
    Off,
    /// MMR 最大边际相关性;`lambda ∈ [0,1]` 越大越重相关性。
    Mmr {
        /// 相关性权重。
        lambda: f32,
    },
}

/// 关系类型编号:内置占用 `0..=15`(当前 `0..=3`),自定义从 16 起(L2 注册表)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelationKind(pub u16);

impl RelationKind {
    /// `=0` 派生自(摘要 → 来源)。
    pub const DERIVED_FROM: Self = Self(0);
    /// `=1` 支持。
    pub const SUPPORTS: Self = Self(1);
    /// `=2` 矛盾。
    pub const CONTRADICTS: Self = Self(2);
    /// `=3` 弱相关。
    pub const RELATED: Self = Self(3);
    /// 自定义关系类型的最小编号。
    pub const FIRST_CUSTOM: u16 = 16;
}

/// 关系邻接索引方向(L2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RelationIndex {
    /// 仅出边索引(默认)。
    #[default]
    Outgoing,
    /// 出边 + 反向边索引(空间 ×2)。
    Both,
}

/// 检索反馈(L4)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feedback {
    /// 结果被使用。
    Used,
    /// 结果被忽略。
    Ignored,
    /// 结果被修正为指向另一条记录。
    Corrected {
        /// 修正后的目标记录。
        by: RowId,
    },
}

/// 一次检索的幂等标识(L4),反馈时原样回传。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueryId(pub u64);

/// 局部更新补丁;外层 `None` = 不改动该字段,`Some(None)` = 清空。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UpdatePatch {
    /// 新向量;`None` = 不改动。
    pub vector: Option<Vec<f32>>,
    /// 新文本;`Some(None)` = 清空。
    pub text: Option<Option<String>>,
    /// 新元数据;`Some(None)` = 清空,`Some(Some(v))` = 整体替换。
    pub metadata: Option<Option<Meta>>,
    /// 新重要度。
    pub importance: Option<f32>,
    /// 新 TTL;`Some(None)` = 取消过期。
    pub ttl: Option<Option<Duration>>,
    /// 新有效时间区间 `(valid_from, valid_to)`。
    pub valid_time: Option<(i64, Option<i64>)>,
    /// 新可信度。
    pub confidence: Option<f32>,
    /// 新来源 / 派生链;`Some(None)` = 清空。
    pub provenance: Option<Option<Meta>>,
}

impl UpdatePatch {
    /// 返回一个不改动任何字段的空补丁。
    pub fn new() -> Self {
        Self::default()
    }
}

/// 文本 / 元数据压缩策略(实现见 L2 可选 feature)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compression {
    /// 不压缩(默认)。
    #[default]
    None,
    /// 内置 LZ4 风格压缩(feature `compress`)。
    Lz4,
    /// Zstd 压缩(feature `compress-zstd`)。
    Zstd,
}

/// 时间源:TTL / 遗忘曲线 / `touch` 一律经此取"当前 Unix 毫秒"。
///
/// 生产用 [`SystemClock`];测试注入可回拨 / 快进的假时钟,保证确定性。
pub trait Clock: Send + Sync {
    /// 返回当前 Unix 毫秒时间戳。
    fn now_unix_ms(&self) -> i64;
}

/// 读取系统墙上时钟的时间源。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> i64 {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(elapsed) => elapsed.as_millis() as i64,
            Err(before_epoch) => -(before_epoch.duration().as_millis() as i64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_accepts_bounds_and_rejects_outside() {
        assert_eq!(Dimension::new(Dimension::MIN).unwrap().get(), 1);
        assert_eq!(Dimension::new(Dimension::MAX).unwrap().get(), 65_536);
        assert!(matches!(Dimension::new(0), Err(MnemeError::Invalid(_))));
        assert!(matches!(
            Dimension::new(65_537),
            Err(MnemeError::Invalid(_))
        ));
    }

    #[test]
    fn defaults_match_design_table() {
        assert_eq!(
            FsyncPolicy::default(),
            FsyncPolicy::Batched(Duration::from_millis(20))
        );
        assert_eq!(InsertMode::default(), InsertMode::Upsert);
        assert_eq!(VectorFormat::default(), VectorFormat::F32);

        let hnsw = HnswParams::default();
        assert_eq!(
            (hnsw.m, hnsw.m0, hnsw.ef_construction, hnsw.ef_search),
            (16, 32, 200, 64)
        );

        let limits = Limits::default();
        assert_eq!(limits.key_bytes, 1024);
        assert_eq!(limits.text_bytes, 1024 * 1024);
        assert_eq!(limits.meta_bytes, 64 * 1024);
        assert_eq!(limits.top_k_max, 4096);
        assert_eq!(limits.ef_max, 4096);

        let compaction = CompactionPolicy::default();
        assert_eq!(compaction.tier_ratio, 4);
        assert_eq!(compaction.segment_rows, 8192);
        assert_eq!(compaction.history_horizon, None);

        let scoring = Scoring::default();
        assert_eq!(scoring.w_sim, 1.0);
        assert_eq!(scoring.w_recency, 0.0);
        assert_eq!(scoring.c_norm, 100);
        assert_eq!(scoring, Scoring::new());
    }

    #[test]
    fn relation_kind_builtin_numbers_are_stable() {
        assert_eq!(RelationKind::DERIVED_FROM.0, 0);
        assert_eq!(RelationKind::SUPPORTS.0, 1);
        assert_eq!(RelationKind::CONTRADICTS.0, 2);
        assert_eq!(RelationKind::RELATED.0, 3);
        assert_eq!(RelationKind::FIRST_CUSTOM, 16);
    }

    #[test]
    fn update_patch_default_changes_nothing() {
        let patch = UpdatePatch::new();
        assert!(patch.vector.is_none());
        assert!(patch.text.is_none());
        assert!(patch.metadata.is_none());
        assert!(patch.importance.is_none());
        assert!(patch.ttl.is_none());
        assert!(patch.valid_time.is_none());
        assert!(patch.confidence.is_none());
        assert!(patch.provenance.is_none());
    }

    #[test]
    fn system_clock_returns_positive_unix_ms() {
        let now = SystemClock.now_unix_ms();
        // 2020-01-01 之后,远小于 100 年后的值。
        assert!(now > 1_577_836_800_000);
    }
}
