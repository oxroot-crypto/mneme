use crate::core::metric::{Metric, Score};
use crate::core::options::{Scoring, TimeAxis};
use crate::memory::search::Scored;
use crate::memory::table::{AccessStat, ReaderView, SlotData};

use super::types::ScoreBreakdown;

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
/// 综合重排输入(聚合视图/候选/策略/度量,避免超长参数列表)。
pub(crate) struct CompositeRerank<'a> {
    /// 只读视图(取记录体与访问统计)。
    pub(crate) view: &'a ReaderView,
    /// 待重排候选。
    pub(crate) candidates: &'a [Scored],
    /// 综合打分权重。
    pub(crate) scoring: &'a Scoring,
    /// 距离度量。
    pub(crate) metric: Metric,
    /// 当前时刻(Unix 毫秒)。
    pub(crate) now_ms: i64,
}

/// 对候选做综合重排,返回按综合分降序(同分 `RowId` 升序)的结果。
pub(crate) fn rerank_composite(input: CompositeRerank<'_>) -> Vec<(Scored, ScoreBreakdown)> {
    let CompositeRerank {
        view,
        candidates,
        scoring,
        metric,
        now_ms,
    } = input;
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
