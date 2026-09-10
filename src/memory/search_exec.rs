//! `SearchBuilder` 的检索执行流水线(`search_exec.rs`)。
//!
//! 承载 `execute()` 的内部阶段:视图准备、查询校验、暴力扫描、关系联想扩展、
//! 综合打分、结果级去重/多样性与精排。公开入口 `execute()` 定义在
//! [`search_builder`](crate::memory::search_builder) 模块;本模块方法均为
//! `pub(super)`,仅供其调用。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::error::{MnemeError, Result};
use crate::core::options::{Diversity, QueryId};
use crate::core::types::{NsId, RowId};
use crate::memory::expand::{self, ExpandCtx, expand_candidates};
use crate::memory::record::Hit;
use crate::memory::relation::Edge;
use crate::memory::rerank::QueryCtx;
use crate::memory::score::{self, ScoreBreakdown};
use crate::memory::search::{self, Scored};
use crate::memory::search_builder::SearchBuilder;
use crate::memory::table::ReaderView;
use crate::memory::temporal;

/// 全局查询标识分配器(`execute()` 缺省生成 `QueryId`)。
static NEXT_QUERY_ID: AtomicU64 = AtomicU64::new(1);

impl SearchBuilder<'_> {
    /// 取检索视图:优先钉住的快照,否则取当前读视图;并校验关闭/文本/融合通道。
    pub(super) fn prepare_view(&self) -> Result<Arc<ReaderView>> {
        let view = match &self.pinned {
            Some(view) => Arc::clone(view),
            None => self.table.view(),
        };
        if view.closed {
            return Err(MnemeError::Closed);
        }
        if self.text.is_some() {
            return Err(MnemeError::Unsupported {
                feature: "BM25 文本检索(L4)",
            });
        }
        // Fusion 属于 L4 双通道融合能力:单独设置即拒绝,绝不静默忽略(FC-MEM-ERR-002)。
        if self.fusion.is_some() {
            return Err(MnemeError::Unsupported {
                feature: "双通道融合(Fusion, L4)",
            });
        }
        Ok(view)
    }

    /// 校验查询向量维度与 `top_k`/`ef` 上限。
    pub(super) fn validate_query(&self, query: &[f32]) -> Result<()> {
        let expected = self.config.dimension.get() as usize;
        if query.len() != expected {
            return Err(MnemeError::DimensionMismatch {
                expected: self.config.dimension.get(),
                got: query.len(),
            });
        }
        if self.top_k > self.config.limits.top_k_max as usize {
            return Err(MnemeError::LimitExceeded {
                field: "top_k",
                limit: self.config.limits.top_k_max as usize,
                got: self.top_k,
            });
        }
        if let Some(ef) = self.ef
            && ef > self.config.limits.ef_max as usize
        {
            return Err(MnemeError::LimitExceeded {
                field: "ef",
                limit: self.config.limits.ef_max as usize,
                got: ef,
            });
        }
        Ok(())
    }

    /// 校验多样性策略参数:非有限 `lambda` 会让 MMR 的 `clamp` 对 NaN 失效并静默退化
    /// (固定取首项),故入口显式拒绝(FC-MEM-PRE-003,拒绝静默失败)。
    pub(super) fn validate_diversify(&self) -> Result<()> {
        if let Diversity::Mmr { lambda } = self.diversify
            && !lambda.is_finite()
        {
            return Err(MnemeError::Config {
                reason: "MMR lambda 必须是 [0,1] 内的有限值",
            });
        }
        Ok(())
    }

    /// 解析命名空间路径对应的 `NsId`(未注册则 `None`)。
    pub(super) fn resolve_ns_id(&self, view: &ReaderView) -> Option<NsId> {
        view.ns_registry.iter().find_map(|(id, path)| {
            if **path == *self.ns_path {
                Some(*id)
            } else {
                None
            }
        })
    }

    /// 指定 `as_of` 时在版本链上重建历史视图。
    pub(super) fn apply_as_of(&self, view: Arc<ReaderView>) -> Arc<ReaderView> {
        match self.as_of {
            Some(ts_ms) => Arc::new(temporal::snapshot_at(&view, ts_ms)),
            None => view,
        }
    }

    /// 过滤先行 + 暴力扫描。
    pub(super) fn run_search(
        &self,
        view: &ReaderView,
        ns_id: NsId,
        query: &[f32],
        now: i64,
    ) -> Result<Vec<Scored>> {
        search::search(&search::SearchParams {
            view,
            ns_id,
            query,
            metric: self.config.metric,
            top_k: self.top_k,
            filter: self.filter.as_ref(),
            now_ms: now,
            block: self.config.tuning.parallel_block,
            parallelism: self.config.parallelism,
        })
    }

    /// 生成查询幂等标识。
    pub(super) fn resolve_query_id(&self) -> QueryId {
        self.query_id
            .unwrap_or_else(|| QueryId(NEXT_QUERY_ID.fetch_add(1, Ordering::Relaxed)))
    }

    /// 沿关系边做联想扩展,返回扩展后的候选与来源边映射。
    pub(super) fn apply_expansion(
        &self,
        view: &ReaderView,
        mut scored: Vec<Scored>,
        now: i64,
    ) -> (Vec<Scored>, HashMap<RowId, Edge>) {
        let mut via_map: HashMap<RowId, Edge> = HashMap::new();
        if let Some(expand) = &self.expand {
            let ctx = ExpandCtx {
                view,
                expand,
                filter: self.filter.as_ref(),
                now,
            };
            for (rowid, score_value, edge) in expand_candidates(&ctx, &scored) {
                if let Some(slot) = view.live_slot(rowid) {
                    via_map.insert(rowid, edge);
                    scored.push(Scored {
                        slot,
                        rowid,
                        score: score_value,
                    });
                }
            }
        }
        (scored, via_map)
    }

    /// 综合打分重排(未开启 `Scoring` 时退化为纯相似度)。
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
