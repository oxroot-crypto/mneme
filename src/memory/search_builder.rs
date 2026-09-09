//! 检索构建器与精排/融合钩子(`search_builder.rs`)。
//!
//! 承载 `SearchBuilder` 的链式配置与 `execute()` 执行流程,以及关系联想扩展、
//! 结果级去重的内部辅助。公开签名在 L1 冻结(设计 03 §2.2)。

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::error::{MnemeError, Result};
use crate::core::options::{Diversity, QueryId, Scoring};
use crate::core::types::{NsId, RowId};
use crate::memory::config::Config;
use crate::memory::dedup::ResultDedup;
use crate::memory::expand::{self, ExpandCtx, expand_candidates};
use crate::memory::pred::Expr;
use crate::memory::record::Hit;
use crate::memory::relation::{Edge, RelationExpand};
use crate::memory::score::{self, ScoreBreakdown};
use crate::memory::search::{self, Scored};
use crate::memory::table::{ReaderView, Table};
use crate::memory::temporal;

/// 全局查询标识分配器(`execute()` 缺省生成 `QueryId`)。
static NEXT_QUERY_ID: AtomicU64 = AtomicU64::new(1);

/// Reciprocal Rank Fusion 的缺省平滑常数。
const RRF_K: u32 = 60;

/// 融合器(双通道检索;L1 仅记录,`.text()` 在 L4 前返回 `Invalid`)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fusion {
    /// Reciprocal Rank Fusion,默认 `k=60`。
    Rrf {
        /// 平滑常数。
        k: u32,
    },
    /// 加权融合,`alpha ∈ [0,1]`。
    Weighted {
        /// 向量通道权重。
        alpha: f32,
    },
}

impl Default for Fusion {
    fn default() -> Self {
        Self::Rrf { k: RRF_K }
    }
}

/// 精排钩子的查询上下文。
#[derive(Debug, Clone, Copy)]
pub struct QueryCtx<'a> {
    /// 查询文本。
    pub text: Option<&'a str>,
    /// 查询向量。
    pub vector: Option<&'a [f32]>,
}

/// 精排钩子。
pub trait Reranker: Send + Sync {
    /// 对命中列表重排;实现不得改变命中集合语义。
    fn rerank(&self, ctx: &QueryCtx<'_>, hits: Vec<Hit>) -> Vec<Hit>;
}

/// 检索构建器。
pub struct SearchBuilder<'a> {
    pub(crate) table: Arc<Table>,
    pub(crate) config: Arc<Config>,
    pub(crate) ns_path: Arc<str>,
    pub(crate) pinned: Option<Arc<ReaderView>>,
    pub(crate) vector: Option<Vec<f32>>,
    pub(crate) text: Option<String>,
    pub(crate) top_k: usize,
    pub(crate) ef: Option<usize>,
    pub(crate) filter: Option<Expr>,
    pub(crate) dedup: ResultDedup,
    pub(crate) fusion: Fusion,
    pub(crate) scoring: Option<Scoring>,
    pub(crate) diversify: Diversity,
    pub(crate) expand: Option<RelationExpand>,
    pub(crate) as_of: Option<i64>,
    pub(crate) query_id: Option<QueryId>,
    pub(crate) rerank: Option<Arc<dyn Reranker>>,
    pub(crate) _marker: PhantomData<&'a ()>,
}

impl SearchBuilder<'_> {
    /// 设置查询向量。
    pub fn vector(mut self, query: &[f32]) -> Self {
        self.vector = Some(query.to_vec());
        self
    }

    /// 设置查询文本(L4 前 `execute()` 返回 `Unsupported`)。
    pub fn text(mut self, query: &str) -> Self {
        self.text = Some(query.to_string());
        self
    }

    /// 设置返回条数(默认 10,上限 [`Limits::top_k_max`](crate::Limits::top_k_max))。
    pub fn top_k(mut self, top_k: usize) -> Self {
        self.top_k = top_k;
        self
    }

    /// 设置探查宽度(仅 L3+ 生效;超上限返回 `LimitExceeded`)。
    pub fn ef(mut self, ef: usize) -> Self {
        self.ef = Some(ef);
        self
    }

    /// 设置预过滤表达式。
    pub fn filter(mut self, filter: Expr) -> Self {
        self.filter = Some(filter);
        self
    }

    /// 设置结果级去重。
    pub fn dedup(mut self, dedup: ResultDedup) -> Self {
        self.dedup = dedup;
        self
    }

    /// 设置双通道融合器(L4 前 `execute()` 返回 `Unsupported`)。
    pub fn fusion(mut self, fusion: Fusion) -> Self {
        self.fusion = fusion;
        self
    }

    /// 开启综合打分。
    pub fn score(mut self, scoring: Scoring) -> Self {
        self.scoring = Some(scoring);
        self
    }

    /// 开启多样性。
    pub fn diversify(mut self, diversify: Diversity) -> Self {
        self.diversify = diversify;
        self
    }

    /// 开启关系联想扩展。
    pub fn expand(mut self, expand: RelationExpand) -> Self {
        self.expand = Some(expand);
        self
    }

    /// 指定事务时间上界,做历史检索。
    pub fn as_of(mut self, ts_ms: i64) -> Self {
        self.as_of = Some(ts_ms);
        self
    }

    /// 指定查询幂等标识;缺省由 `execute()` 生成。
    pub fn query_id(mut self, query_id: QueryId) -> Self {
        self.query_id = Some(query_id);
        self
    }

    /// 设置精排钩子。
    pub fn rerank(mut self, rerank: Arc<dyn Reranker>) -> Self {
        self.rerank = Some(rerank);
        self
    }

    /// 执行检索。
    ///
    /// # Errors
    /// * 无查询通道 → [`MnemeError::Config`];
    /// * 设置 `text`/`Fusion`(L4 前未实现)→ [`MnemeError::Unsupported`];
    /// * 查询向量维度不符 → [`MnemeError::DimensionMismatch`];
    /// * `top_k`/`ef` 超上限 → [`MnemeError::LimitExceeded`];
    /// * 库已关闭 → [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// let hits = ns.search().vector(&[1.0, 0.0]).top_k(1).execute().unwrap();
    /// assert_eq!(hits.len(), 1);
    /// ```
    pub fn execute(&self) -> Result<Vec<Hit>> {
        let view = self.prepare_view()?;
        let Some(query) = &self.vector else {
            return Err(MnemeError::Config {
                reason: "检索至少需要一个查询通道",
            });
        };
        self.validate_query(query)?;
        let Some(ns_id) = self.resolve_ns_id(&view) else {
            return Ok(Vec::new());
        };
        let now = self.config.clock.now_unix_ms();
        let view = self.apply_as_of(view);
        let scored = self.run_search(&view, ns_id, query, now);
        let (scored, via_map) = self.apply_expansion(&view, scored, now);
        let ranked = self.rank(&view, scored, now);
        let hits = self.build_hits(&view, ranked, self.resolve_query_id(), &via_map);
        Ok(self.apply_rerank(hits))
    }

    /// 取检索视图:优先钉住的快照,否则取当前读视图;并校验关闭/文本通道。
    fn prepare_view(&self) -> Result<Arc<ReaderView>> {
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
        Ok(view)
    }

    /// 校验查询向量维度与 `top_k`/`ef` 上限。
    fn validate_query(&self, query: &[f32]) -> Result<()> {
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

    /// 解析命名空间路径对应的 `NsId`(未注册则 `None`)。
    fn resolve_ns_id(&self, view: &ReaderView) -> Option<NsId> {
        view.ns_registry.iter().find_map(|(id, path)| {
            if **path == *self.ns_path {
                Some(*id)
            } else {
                None
            }
        })
    }

    /// 指定 `as_of` 时在版本链上重建历史视图。
    fn apply_as_of(&self, view: Arc<ReaderView>) -> Arc<ReaderView> {
        match self.as_of {
            Some(ts_ms) => Arc::new(temporal::snapshot_at(&view, ts_ms)),
            None => view,
        }
    }

    /// 过滤先行 + 暴力扫描。
    fn run_search(&self, view: &ReaderView, ns_id: NsId, query: &[f32], now: i64) -> Vec<Scored> {
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
    fn resolve_query_id(&self) -> QueryId {
        self.query_id
            .unwrap_or_else(|| QueryId(NEXT_QUERY_ID.fetch_add(1, Ordering::Relaxed)))
    }

    /// 沿关系边做联想扩展,返回扩展后的候选与来源边映射。
    fn apply_expansion(
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
    fn rank(
        &self,
        view: &ReaderView,
        scored: Vec<Scored>,
        now: i64,
    ) -> Vec<(Scored, ScoreBreakdown)> {
        match &self.scoring {
            Some(scoring) => {
                score::rerank_composite(view, &scored, scoring, self.config.metric, now)
            }
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
    fn build_hits(
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
    fn apply_rerank(&self, mut hits: Vec<Hit>) -> Vec<Hit> {
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

impl std::fmt::Debug for SearchBuilder<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchBuilder")
            .field("top_k", &self.top_k)
            .field("has_vector", &self.vector.is_some())
            .field("has_filter", &self.filter.is_some())
            .finish_non_exhaustive()
    }
}
