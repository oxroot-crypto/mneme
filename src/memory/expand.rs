//! 关系联想扩展与结果级去重(`expand.rs`)。
//!
//! 从 `search_builder.rs` 拆出的检索后处理:沿关系边逐跳扩展候选,
//! 以及对 `execute()` 结果做 `ResultDedup` 去重。不构成公开 API。
//! 扩展只沿同命名空间的边推进,绝不把其他命名空间的记忆引入结果
//! (`FC-SCORE-POST-004`):即使 `relate` 记录了跨命名空间边,扩展也视其不存在。

use std::collections::HashSet;

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
}

/// 逐节点推进的关系扩展器。
struct Expander<'a> {
    view: &'a ReaderView,
    ns_id: NsId,
    expand: &'a RelationExpand,
    filter: Option<&'a Expr>,
    now: i64,
    visited: HashSet<RowId>,
    result: Vec<(RowId, f32, Edge)>,
}

impl<'a> Expander<'a> {
    fn new(ctx: &ExpandCtx<'a>, seeds: &[Scored]) -> Self {
        Self {
            view: ctx.view,
            ns_id: ctx.ns_id,
            expand: ctx.expand,
            filter: ctx.filter,
            now: ctx.now,
            visited: seeds.iter().map(|seed| seed.rowid).collect(),
            result: Vec::new(),
        }
    }

    /// 从种子集合出发逐跳扩展,返回 `(rowid, score, edge)` 列表。
    fn run(mut self, seeds: &[Scored]) -> Vec<(RowId, f32, Edge)> {
        let mut frontier: Vec<(RowId, f32)> =
            seeds.iter().map(|seed| (seed.rowid, seed.score)).collect();
        for _ in 0..self.expand.hops.min(MAX_EXPAND_HOPS) {
            let mut next = Vec::new();
            for (from, seed_score) in &frontier {
                self.visit(*from, *seed_score, &mut next);
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        self.result
    }

    /// 展开单个节点的出边,命中者写入 `self.result` 与下一跳 `next`。
    fn visit(&mut self, from: RowId, seed_score: f32, next: &mut Vec<(RowId, f32)>) {
        let Some(edges) = self.view.out_edges.get(&from) else {
            return;
        };
        for edge in edges {
            if !self.expand.kinds.is_empty() && !self.expand.kinds.contains(&edge.kind) {
                continue;
            }
            // `visited` 与 `result` 同受 `max_nodes` 封顶:否则大量被命名空间/存活/
            // 过滤拒绝的边会持续占用 `visited`,空间上界退化为 O(边数)
            // (FC-SCORE-CPLX-002 声明空间 O(max_nodes))。
            if self.result.len() >= self.expand.max_nodes
                || self.visited.len() >= self.expand.max_nodes
            {
                return;
            }
            if !self.visited.insert(edge.to) {
                continue;
            }
            let Some(slot) = self.view.live_slot(edge.to) else {
                continue;
            };
            let slot_data = &self.view.slots[slot.get() as usize];
            if slot_data.ns_id != self.ns_id {
                continue;
            }
            if !slot_data.is_live(self.now) {
                continue;
            }
            if let Some(expr) = self.filter {
                let ctx = EvalCtx {
                    slot: slot_data,
                    access: self.view.access.get(&edge.to).copied(),
                };
                if !pred::matches(expr, &ctx) {
                    continue;
                }
            }
            let score_value = seed_score * edge.weight * self.expand.decay;
            self.result.push((edge.to, score_value, edge.clone()));
            next.push((edge.to, score_value));
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
            for candidate in ranked {
                let vector = &view.slots[candidate.0.slot.get() as usize].vector;
                let near = kept.iter().any(|(existing, _)| {
                    score::cosine_sim(&view.slots[existing.slot.get() as usize].vector, vector)
                        >= threshold
                });
                if !near {
                    kept.push(candidate);
                }
            }
            kept
        }
    }
}
