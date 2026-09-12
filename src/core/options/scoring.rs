//! 记忆感知排序与关系类型(L4 / L9)。
//!
//! 覆盖综合打分配置、新鲜度时间轴、结果多样性、检索反馈与幂等查询标识,
//! 以及记忆关系类型与邻接索引方向(语义见设计 10 与 09 §2.2)。

use std::time::Duration;

use crate::core::types::RowId;

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
    ///
    /// **尚未落地**:查询入口对 `true` 返回 `Unsupported`(拒绝静默忽略),
    /// 见设计 10 §2.3 与 `FC-MEM-ERR-002`。
    pub bias_routing: bool,
}

impl Scoring {
    /// 返回默认配置(等价于 [`Scoring::default`])。
    ///
    /// # Returns
    ///
    /// 仅相似度权重为 1.0、其余因子全部关闭的默认配置。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoring_defaults_match_design() {
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
}
