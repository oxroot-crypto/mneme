//! 通道后处理:关系扩展、综合打分、去重/多样性、截断与精排(设计 06 §5)。

use std::collections::HashMap;

use crate::core::options::{Diversity, QueryId};
use crate::core::types::{NsId, RowId, SlotId};
use crate::memory::QueryCtx;
use crate::memory::expand::{self, ExpandCtx, expand_candidates};
use crate::memory::record::Hit;
use crate::memory::relation::Edge;
use crate::memory::score::{self, ScoreBreakdown};
use crate::memory::search::Scored;
use crate::memory::search_builder::SearchBuilder;
use crate::memory::table::ReaderView;

impl SearchBuilder<'_> {
    /// 沿关系边做联想扩展,返回扩展后的候选与来源边映射。
    ///
    /// 扩展只在本命名空间内推进,跨命名空间边视为不存在(`FC-SCORE-POST-004`)。
    /// 与既有通道候选按 `RowId` 合并,分数取 `max(自身分, boost)`,同一记录绝不
    /// 重复出现;`via` 只在扩展确有贡献(新增候选或提升分数)时记录
    /// (`FC-SCORE-POST-006`)。
    pub(super) fn apply_expansion(
        &self,
        view: &ReaderView,
        ns_id: NsId,
        mut scored: Vec<Scored>,
        now: i64,
    ) -> (Vec<Scored>, HashMap<RowId, Edge>) {
        let mut via_map: HashMap<RowId, Edge> = HashMap::new();
        if self.expand.is_none() {
            return (scored, via_map);
        }
        let mut index_of: HashMap<RowId, usize> = scored
            .iter()
            .enumerate()
            .map(|(index, candidate)| (candidate.rowid, index))
            .collect();
        // reason: `expand` 已在上方判空;此处取得引用供 `ExpandCtx` 使用。
        let Some(expand) = &self.expand else {
            return (scored, via_map);
        };
        let ctx = ExpandCtx {
            view,
            ns_id,
            expand,
            filter: self.filter.as_ref(),
            now,
            uses_access: self
                .filter
                .as_ref()
                .is_some_and(crate::memory::pred::Expr::uses_access),
        };
        for (rowid, boost, edge) in expand_candidates(&ctx, &scored) {
            let Some(slot) = view.live_slot(rowid) else {
                continue;
            };
            merge_expanded(
                &mut scored,
                &mut index_of,
                &mut via_map,
                (rowid, boost, edge, slot),
            );
        }
        (scored, via_map)
    }

    /// 综合打分重排(未开启 `Scoring` 时退化为纯通道分)。
    pub(super) fn rank(
        &self,
        view: &ReaderView,
        scored: Vec<Scored>,
        now: i64,
    ) -> Vec<(Scored, ScoreBreakdown)> {
        match &self.scoring {
            Some(scoring) => score::rerank_composite(score::CompositeRerank {
                view,
                candidates: &scored,
                scoring,
                metric: self.config.metric,
                now_ms: now,
            }),
            None => scored
                .iter()
                .map(|candidate| {
                    (
                        *candidate,
                        ScoreBreakdown {
                            sim: candidate.score,
                            ..ScoreBreakdown::default()
                        },
                    )
                })
                .collect(),
        }
    }

    /// 结果级去重 + MMR 多样性 + 截断,并物化为 `Hit`。
    pub(super) fn build_hits(
        &self,
        view: &ReaderView,
        mut ranked: Vec<(Scored, ScoreBreakdown)>,
        query_id: QueryId,
        via_map: &HashMap<RowId, Edge>,
    ) -> Vec<Hit> {
        ranked = expand::apply_result_dedup(view, ranked, self.dedup);
        if let Diversity::Mmr { lambda } = self.diversify {
            ranked = score::mmr_select(view, ranked, lambda, self.top_k);
        }
        ranked.truncate(self.top_k);
        ranked
            .into_iter()
            .map(|(candidate, breakdown)| {
                let slot_data = &view.slots[candidate.slot.get() as usize];
                Hit {
                    rowid: candidate.rowid,
                    query_id,
                    key: slot_data.key.clone(),
                    score: candidate.score,
                    created_at: slot_data.created_at,
                    expires_at: slot_data.expires_at,
                    importance: slot_data.importance,
                    confidence: slot_data.confidence,
                    valid_from: slot_data.valid_from,
                    valid_to: slot_data.valid_to,
                    text: slot_data.text.as_ref().map(|text| text.to_string()),
                    metadata: slot_data.meta.clone(),
                    provenance: slot_data.provenance.clone(),
                    via: via_map.get(&candidate.rowid).cloned(),
                    breakdown: Some(breakdown),
                }
            })
            .collect()
    }

    /// 精排钩子(未设置时原样返回)。
    pub(super) fn apply_rerank(&self, mut hits: Vec<Hit>) -> Vec<Hit> {
        if let Some(reranker) = &self.rerank {
            let ctx = QueryCtx {
                text: self.text.as_deref(),
                vector: self.vector.as_deref(),
            };
            hits = reranker.rerank(&ctx, hits);
        }
        hits
    }
}

/// 合并一条扩展候选:命中既有条目时仅在扩展分更高时提升分数并记 `via`;
/// 未命中则追加新候选(`FC-SCORE-POST-006`)。
fn merge_expanded(
    scored: &mut Vec<Scored>,
    index_of: &mut HashMap<RowId, usize>,
    via_map: &mut HashMap<RowId, Edge>,
    (rowid, boost, edge, slot): (RowId, f32, Edge, SlotId),
) {
    match index_of.get(&rowid).copied() {
        Some(index) => {
            let existing = &mut scored[index];
            // max 合并:扩展分仅在更高时提升自身分(不比自身分差时保留原分)。
            if boost.total_cmp(&existing.score) == std::cmp::Ordering::Greater {
                existing.score = boost;
                existing.slot = slot;
                via_map.insert(rowid, edge);
            }
        }
        None => {
            index_of.insert(rowid, scored.len());
            via_map.insert(rowid, edge.clone());
            scored.push(Scored {
                slot,
                rowid,
                score: boost,
            });
        }
    }
}
