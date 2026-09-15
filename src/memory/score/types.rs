use std::sync::Arc;

use crate::core::types::RowId;
use crate::memory::pred::Expr;
use crate::memory::record::{Record, RecordRef};

/// 沉淀聚类的缺省相似度阈值。
const DEFAULT_CONSOLIDATION_THRESHOLD: f32 = 0.95;

/// 单簇成员数的缺省上限。
const DEFAULT_MAX_CLUSTER: usize = 32;

/// 综合打分的各因子贡献(调试/审计用)。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScoreBreakdown {
    /// 归一化相似度贡献。
    pub sim: f32,
    /// 新鲜度贡献。
    pub recency: f32,
    /// 重要度贡献。
    pub importance: f32,
    /// 访问频次贡献。
    pub access: f32,
    /// 可信度贡献。
    pub confidence: f32,
    /// 关系联想 boost。
    pub boost: f32,
}

/// 记忆沉淀的摘要器回调。
pub trait Summarizer: Send + Sync {
    /// 对一簇来源记录生成摘要记录;返回 `None` 时由引擎拼接。
    fn summarize(&self, cluster: &[RecordRef<'_>]) -> Option<Record>;
}

/// 记忆沉淀策略(见设计 09 §5)。
pub struct ConsolidationPolicy {
    /// 候选范围,`None` = 当前命名空间全部活记录。
    pub filter: Option<Expr>,
    /// 近似重复阈值,默认 0.95;`[0,1]` 内的有限值,越界在 `consolidate` 入口拒绝。
    pub threshold: f32,
    /// 单簇上限,默认 32。
    pub max_cluster: usize,
    /// 摘要写入的命名空间路径,`None` = 调用方所在命名空间。
    pub target: Option<String>,
    /// 宿主摘要器,`None` = 引擎拼接。
    pub summarizer: Option<Arc<dyn Summarizer>>,
    /// 是否保留来源(以 `DERIVED_FROM` 边指向摘要),默认 `true`。
    pub keep_sources: bool,
}

impl Default for ConsolidationPolicy {
    fn default() -> Self {
        Self {
            filter: None,
            threshold: DEFAULT_CONSOLIDATION_THRESHOLD,
            max_cluster: DEFAULT_MAX_CLUSTER,
            target: None,
            summarizer: None,
            keep_sources: true,
        }
    }
}

impl std::fmt::Debug for ConsolidationPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsolidationPolicy")
            .field("threshold", &self.threshold)
            .field("max_cluster", &self.max_cluster)
            .field("target", &self.target)
            .field("keep_sources", &self.keep_sources)
            .finish_non_exhaustive()
    }
}

/// `consolidate` 的执行报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsolidateReport {
    /// 发生沉淀的簇数(单元素簇不计)。
    pub clusters: usize,
    /// 被合并的来源记录总数。
    pub merged: usize,
    /// 新生成摘要记录的 `RowId`。
    pub created: Vec<RowId>,
}
