//! 记忆感知排序与沉淀(`score.rs`)。
//!
//! L1 提供最小内存实现:综合打分(相似度/新鲜度/重要度/访问/可信度)、
//! MMR 多样性、以及基于连通分量的记忆沉淀。默认全部关闭,行为退化为纯相似度。

use std::sync::Arc;

use crate::core::metric::{Metric, Score};
use crate::core::options::{Scoring, TimeAxis};
use crate::core::simd;
use crate::core::types::RowId;
use crate::memory::pred::Expr;
use crate::memory::record::{Record, RecordRef};
use crate::memory::search::Scored;
use crate::memory::table::{AccessStat, ReaderView, SlotData};

/// 沉淀聚类的缺省相似度阈值。
const DEFAULT_CONSOLIDATION_THRESHOLD: f32 = 0.95;

/// 单簇成员数的缺省上限。
const DEFAULT_MAX_CLUSTER: usize = 32;

/// 余弦相似度分母的零向量判定阈值。
const COSINE_EPSILON: f32 = 1e-12;

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
    /// 近似重复阈值,默认 0.95。
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

/// 余弦相似度(零向量返回 0)。
pub(crate) fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    let denom = (simd::dot(a, a) * simd::dot(b, b)).sqrt();
    if denom < COSINE_EPSILON {
        0.0
    } else {
        simd::dot(a, b) / denom
    }
}

/// 求候选分数的归一化区间(Euclidean 取负后口径统一为"越大越优")。
fn normalize_span(candidates: &[Scored], metric: Metric) -> (f32, f32) {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for candidate in candidates {
        let star = to_star(metric, candidate.score);
        min = min.min(star);
        max = max.max(star);
    }
    (min, max)
}

/// Euclidean 分数越小越近,取负统一到"越大越优"的排序口径。
fn to_star(metric: Metric, score: Score) -> f32 {
    if metric == Metric::Euclidean {
        -score
    } else {
        score
    }
}

/// 单候选综合打分器(聚合视图/策略/归一化区间,避免超长参数列表)。
struct CompositeRanker<'a> {
    view: &'a ReaderView,
    scoring: &'a Scoring,
    metric: Metric,
    /// 相似度归一化下界(已统一到"越大越优"口径)。
    min: f32,
    /// 相似度归一化区间宽度;`0` 表示所有候选同分(归一化为 1)。
    span: f32,
    now_ms: i64,
}

impl CompositeRanker<'_> {
    /// 相似度归一化贡献(`[0,1]`)。
    fn sim_norm(&self, candidate: &Scored) -> f32 {
        if self.span > 0.0 {
            ((to_star(self.metric, candidate.score) - self.min) / self.span).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// 新鲜度因子:距最近有效时间按半衰期指数衰减。
    fn recency(&self, slot_data: &SlotData, access: &AccessStat) -> f32 {
        let last_access = if access.access_count == 0 {
            slot_data.created_at
        } else {
            access.last_access_ms
        };
        let anchor = match self.scoring.time_axis {
            TimeAxis::ValidTime => slot_data.valid_from.max(last_access),
            TimeAxis::TransactionTime => slot_data.created_at.max(last_access),
        };
        let age = (self.now_ms - anchor).max(0) as f64;
        let half_life_ms = self.scoring.half_life.as_millis().max(1) as f64;
        2.0_f64.powf(-age / half_life_ms) as f32
    }

    /// 访问频次因子:对数归一,基准 `c_norm`。
    fn access_factor(&self, access: &AccessStat) -> f32 {
        let c_norm = f64::from(self.scoring.c_norm.max(1));
        (((1.0 + f64::from(access.access_count)).ln() / (1.0 + c_norm).ln()).clamp(0.0, 1.0)) as f32
    }

    /// 打一个候选:各因子加权求和;相似度低于保底 `floor` 时综合分清零。
    fn score_candidate(&self, candidate: &Scored) -> (Scored, ScoreBreakdown) {
        let slot_data = &self.view.slots[candidate.slot.get() as usize];
        let access = self
            .view
            .access
            .get(&candidate.rowid)
            .copied()
            .unwrap_or_default();
        let sim_norm = self.sim_norm(candidate);
        let breakdown = ScoreBreakdown {
            sim: self.scoring.w_sim * sim_norm,
            recency: self.scoring.w_recency * self.recency(slot_data, &access),
            importance: self.scoring.w_importance * slot_data.importance,
            access: self.scoring.w_access * self.access_factor(&access),
            confidence: self.scoring.w_confidence * slot_data.confidence,
            boost: 0.0,
        };
        let mut total = breakdown.sim
            + breakdown.recency
            + breakdown.importance
            + breakdown.access
            + breakdown.confidence;
        if sim_norm < self.scoring.floor {
            total = 0.0;
        }
        (
            Scored {
                score: total,
                ..*candidate
            },
            breakdown,
        )
    }
}

/// 对候选做综合重排,返回按综合分降序(同分 `RowId` 升序)的结果。
pub(crate) fn rerank_composite(
    view: &ReaderView,
    candidates: &[Scored],
    scoring: &Scoring,
    metric: Metric,
    now_ms: i64,
) -> Vec<(Scored, ScoreBreakdown)> {
    let (min, max) = normalize_span(candidates, metric);
    let ranker = CompositeRanker {
        view,
        scoring,
        metric,
        min,
        span: max - min,
        now_ms,
    };
    let mut ranked: Vec<(Scored, ScoreBreakdown)> = candidates
        .iter()
        .map(|candidate| ranker.score_candidate(candidate))
        .collect();
    ranked.sort_by(|a, b| {
        b.0.score
            .partial_cmp(&a.0.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.rowid.cmp(&b.0.rowid))
    });
    ranked
}

/// 以 MMR 贪心选出至多 `k` 条,保证相关且互相不同。
pub(crate) fn mmr_select(
    view: &ReaderView,
    mut candidates: Vec<(Scored, ScoreBreakdown)>,
    lambda: f32,
    k: usize,
) -> Vec<(Scored, ScoreBreakdown)> {
    let lambda = lambda.clamp(0.0, 1.0);
    let mut selected: Vec<(Scored, ScoreBreakdown)> = Vec::new();
    while selected.len() < k && !candidates.is_empty() {
        let mut best_idx = 0;
        let mut best_score = f32::NEG_INFINITY;
        for (idx, candidate) in candidates.iter().enumerate() {
            let redundancy = selected
                .iter()
                .map(|chosen| {
                    cosine_sim(
                        &view.slots[chosen.0.slot.get() as usize].vector,
                        &view.slots[candidate.0.slot.get() as usize].vector,
                    )
                })
                .fold(f32::NEG_INFINITY, f32::max);
            let redundancy = if redundancy == f32::NEG_INFINITY {
                0.0
            } else {
                redundancy
            };
            let mmr = lambda * candidate.0.score - (1.0 - lambda) * redundancy;
            if mmr > best_score {
                best_score = mmr;
                best_idx = idx;
            }
        }
        selected.push(candidates.remove(best_idx));
    }
    selected
}

/// 以并查集把候选按「相似度 ≥ threshold」聚成连通分量。
///
/// 返回每个连通分量的成员下标;单元素簇也会返回,由调用方过滤。
///
/// 复杂度(FC-MODEL-CPLX-002):两两余弦为 $O(n^2\cdot d)$(n = 候选数),并查集近似线性;
/// 空间 $O(n)$。候选规模受单库内存与 `ConsolidationPolicy.filter` 约束,预期 n 为
/// 单命名空间活记录量级;渐进劣化须先修订该契约(FC-GLOBAL-CPLX-001)。
pub(crate) fn cluster_by_similarity(vectors: &[&[f32]], threshold: f32) -> Vec<Vec<usize>> {
    let n = vectors.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for i in 0..n {
        for j in (i + 1)..n {
            if cosine_sim(vectors[i], vectors[j]) >= threshold {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }
    let mut groups: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    groups.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_sim_bounds() {
        assert!((cosine_sim(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine_sim(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine_sim(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }

    #[test]
    fn clusters_by_threshold() {
        let a = [1.0_f32, 0.0];
        let b = [1.0_f32, 0.01];
        let c = [0.0_f32, 1.0];
        let groups = cluster_by_similarity(&[&a, &b, &c], 0.99);
        assert_eq!(groups.len(), 2);
    }
}
