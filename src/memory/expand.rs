//! 关系联想扩展与结果级去重(`expand.rs`)。
//!
//! 从 `search_builder.rs` 拆出的检索后处理:沿关系边逐跳扩展候选,
//! 以及对 `execute()` 结果做 `ResultDedup` 去重。不构成公开 API。
//! 扩展只沿同命名空间的边推进,绝不把其他命名空间的记忆引入结果
//! (`FC-SCORE-POST-004`):即使 `relate` 记录了跨命名空间边,扩展也视其不存在。

use std::collections::HashSet;

use crate::core::simd;
use crate::core::types::{NsId, RowId};
use crate::memory::dedup::ResultDedup;
use crate::memory::pred::{self, EvalCtx, Expr};
use crate::memory::relation::{Edge, MAX_EXPAND_HOPS, RelationExpand};
use crate::memory::score::{self, ScoreBreakdown};
use crate::memory::search::Scored;
use crate::memory::table::ReaderView;

/// 联想扩展的输入上下文。
pub(crate) struct ExpandCtx<'a> {
    pub(crate) view: &'a ReaderView,
    /// 发起检索的命名空间;扩展只在本命名空间内推进(`FC-SCORE-POST-004`)。
    pub(crate) ns_id: NsId,
    pub(crate) expand: &'a RelationExpand,
    pub(crate) filter: Option<&'a Expr>,
    pub(crate) now: i64,
    /// 过滤是否引用访问统计(行级求值按需查访问表)。
    pub(crate) uses_access: bool,
}

/// 逐节点推进的关系扩展器。
///
/// 传播分按多路径取最大:`best(v) = max(种子分, max_p boost(p))`;命中更优路径时
/// 更新并继续传播(有界 `max_nodes` 封顶)。输出覆盖"分数被提升的种子"与"新扩展
/// 节点",由调用方按 `max(自身分, boost)` 合并(`FC-SCORE-POST-006`)。
struct Expander<'a> {
    view: &'a ReaderView,
    ns_id: NsId,
    expand: &'a RelationExpand,
    filter: Option<&'a Expr>,
    now: i64,
    /// 过滤是否引用访问统计(行级求值按需查访问表)。
    uses_access: bool,
    /// 节点当前最优传播分(种子预置为其原始分;键有序保证确定性)。
    best: std::collections::BTreeMap<RowId, f32>,
    /// 种子原始分(用于判定"扩展是否提升")。
    seed_scores: std::collections::BTreeMap<RowId, f32>,
    /// 取得最优分的入边(种子无)。
    via: std::collections::BTreeMap<RowId, Edge>,
}

impl<'a> Expander<'a> {
    fn new(ctx: &ExpandCtx<'a>, seeds: &[Scored]) -> Self {
        let mut best = std::collections::BTreeMap::new();
        let mut seed_scores = std::collections::BTreeMap::new();
        for seed in seeds {
            let entry = best.entry(seed.rowid).or_insert(f32::NEG_INFINITY);
            if seed.score.total_cmp(entry).is_gt() {
                *entry = seed.score;
            }
            seed_scores.insert(seed.rowid, seed.score);
        }
        Self {
            view: ctx.view,
            ns_id: ctx.ns_id,
            expand: ctx.expand,
            filter: ctx.filter,
            now: ctx.now,
            uses_access: ctx.uses_access,
            best,
            seed_scores,
            via: std::collections::BTreeMap::new(),
        }
    }

    /// 从种子集合出发逐跳扩展,返回被扩展提升/引入的 `(rowid, score, edge)` 列表。
    fn run(mut self, seeds: &[Scored]) -> Vec<(RowId, f32, Edge)> {
        let mut frontier: Vec<(RowId, f32)> =
            seeds.iter().map(|seed| (seed.rowid, seed.score)).collect();
        for _ in 0..self.expand.hops.min(MAX_EXPAND_HOPS) {
            let mut next: Vec<(RowId, f32)> = Vec::new();
            for (from, seed_score) in &frontier {
                self.visit(*from, *seed_score, &mut next);
            }
            next.sort_by_key(|(rowid, _)| *rowid);
            next.dedup_by_key(|(rowid, _)| *rowid);
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        self.best
            .into_iter()
            .filter_map(|(rowid, score)| {
                let improved = self
                    .seed_scores
                    .get(&rowid)
                    .is_none_or(|seed| score.total_cmp(seed).is_gt());
                if !improved {
                    return None;
                }
                self.via
                    .get(&rowid)
                    .map(|edge| (rowid, score, edge.clone()))
            })
            .collect()
    }

    /// 展开单个节点的出边:更优路径才更新并继续传播。
    fn visit(&mut self, from: RowId, seed_score: f32, next: &mut Vec<(RowId, f32)>) {
        let Some(edges) = self.view.out_edges.get(&from) else {
            return;
        };
        for edge in edges {
            if !self.expand.kinds.is_empty() && !self.expand.kinds.contains(&edge.kind) {
                continue;
            }
            let candidate_score = seed_score * edge.weight * self.expand.decay;
            let current = self
                .best
                .get(&edge.to)
                .copied()
                .unwrap_or(f32::NEG_INFINITY);
            if !candidate_score.total_cmp(&current).is_gt() {
                continue; // 无提升:已访问且不更优,不再传播
            }
            // 空间封顶(FC-SCORE-CPLX-002):新节点才占额度,已有节点只做提升。
            if !self.best.contains_key(&edge.to) && self.best.len() >= self.expand.max_nodes {
                return;
            }
            let Some(slot) = self.view.live_slot(edge.to) else {
                continue;
            };
            let slot_data = &self.view.slots[slot.get() as usize];
            if slot_data.ns_id != self.ns_id || !slot_data.is_live(self.now) {
                continue;
            }
            if let Some(expr) = self.filter {
                let ctx = EvalCtx {
                    slot: slot_data,
                    access: self
                        .uses_access
                        .then(|| self.view.access.get(&edge.to).copied())
                        .flatten(),
                };
                if !pred::matches(expr, &ctx) {
                    continue;
                }
            }
            self.best.insert(edge.to, candidate_score);
            self.via.insert(edge.to, edge.clone());
            next.push((edge.to, candidate_score));
        }
    }
}

/// 从种子候选沿关系边做联想扩展。
pub(crate) fn expand_candidates(ctx: &ExpandCtx<'_>, seeds: &[Scored]) -> Vec<(RowId, f32, Edge)> {
    Expander::new(ctx, seeds).run(seeds)
}

/// 结果级去重(按 `RowId` 或近似向量)。
pub(crate) fn apply_result_dedup(
    view: &ReaderView,
    ranked: Vec<(Scored, ScoreBreakdown)>,
    dedup: ResultDedup,
) -> Vec<(Scored, ScoreBreakdown)> {
    match dedup {
        ResultDedup::Off => ranked,
        ResultDedup::ById => {
            let mut seen = HashSet::new();
            ranked
                .into_iter()
                .filter(|(candidate, _)| seen.insert(candidate.rowid))
                .collect()
        }
        ResultDedup::Near { threshold } => {
            let mut kept: Vec<(Scored, ScoreBreakdown)> = Vec::new();
            // 已保留向量的自范数缓存:候选自范数各算一次,两两比较复用(结果逐位不变)。
            let mut kept_norms: Vec<f32> = Vec::new();
            for candidate in ranked {
                let vector = &view.slots[candidate.0.slot.get() as usize].vector;
                let norm = simd::dot(vector, vector);
                let near = kept
                    .iter()
                    .zip(&kept_norms)
                    .any(|((existing, _), existing_norm)| {
                        let existing_vector = &view.slots[existing.slot.get() as usize].vector;
                        score::cosine_from_norms(
                            simd::dot(existing_vector, vector),
                            *existing_norm,
                            norm,
                        ) >= threshold
                    });
                if !near {
                    kept.push(candidate);
                    kept_norms.push(norm);
                }
            }
            kept
        }
    }
}
