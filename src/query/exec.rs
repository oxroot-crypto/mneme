//! `SearchBuilder` 的检索执行流水线(设计 06 §5)。
//!
//! 管线顺序固定:**过滤先行 → 双通道各取 top-k → 融合 → (可选)关系扩展 →
//! 综合打分 → 结果去重/多样性 → 精排钩子**。单通道(仅向量/仅文本)直接取
//! `top_k`;双通道各取 `2k` 再融合取 `k`,减少截断遗憾。
//!
//! 本模块承载 [`SearchBuilder::execute`](crate::SearchBuilder::execute) 与各内部
//! 阶段,使 L4 → L1 的依赖方向成立(L1 只定义类型与 setter)。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::{Diversity, QueryId};
use crate::core::types::{NsId, RowId};
use crate::memory::dedup::ResultDedup;
use crate::memory::expand::{self, ExpandCtx, expand_candidates};
use crate::memory::record::Hit;
use crate::memory::relation::Edge;
use crate::memory::score::{self, ScoreBreakdown};
use crate::memory::search::{self, Scored};
use crate::memory::search_builder::SearchBuilder;
use crate::memory::table::ReaderView;
use crate::memory::temporal;
use crate::memory::{Fusion, QueryCtx};

use super::bm25::{self, Bm25Query};
use super::{fusion, plan};

/// 全局查询标识分配器(`execute()` 缺省生成 `QueryId`)。
///
/// `Ordering::Relaxed` 足够:只要求同一计数器不重号,不承担跨线程可见性顺序;
/// `u64` 回绕需 2^64 次查询,按实际规模视为不可达。
static NEXT_QUERY_ID: AtomicU64 = AtomicU64::new(1);

/// 单通道执行的公共上下文(视图/命名空间/时刻/条数/共享候选)。
struct ChannelCtx<'a> {
    view: &'a ReaderView,
    ns_id: NsId,
    now: i64,
    top_k: usize,
    candidates: &'a [u32],
}

impl SearchBuilder<'_> {
    /// 执行检索。
    ///
    /// # Errors
    /// * 无查询通道 → [`MnemeError::Config`];
    /// * `Fusion` 已设置但只有一个通道,或 `Weighted.alpha` 非 `[0,1]` 内有限值
    ///   → [`MnemeError::Config`];
    /// * 查询向量维度不符 → [`MnemeError::DimensionMismatch`];
    /// * `top_k`/`ef` 超上限 → [`MnemeError::LimitExceeded`];
    /// * MMR `lambda` 含非有限值 → [`MnemeError::Config`](`clamp` 对 NaN 失效会静默退化);
    /// * 库已关闭 → [`MnemeError::Closed`]。
    ///
    /// # Returns
    /// 命中列表,至多 `top_k` 条;未设置 `rerank` 时按最终分从优到劣排序。
    /// 命名空间未注册或无命中时返回空 `Vec`。
    ///
    /// `Scoring::bias_routing = true` 时启用 HNSW 前沿遍历偏置(重要度 + 归一化
    /// 访问频次,只改访问顺序、不参与最终打分,`FC-SCORE-POST-007`);无 HNSW
    /// 索引的段走暴力路径,偏置不生效但绝不报错。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a").text("hello")).unwrap();
    /// let hits = ns.search().vector(&[1.0, 0.0]).text("hello").top_k(1).execute().unwrap();
    /// assert_eq!(hits.len(), 1);
    /// ```
    pub fn execute(&self) -> Result<Vec<Hit>> {
        // 查询延迟采样(固定 32 桶直方图;失败查询同样计入,便于定位慢路径)。
        let started = std::time::Instant::now();
        let result = self.execute_inner();
        let took = started.elapsed();
        self.table.record_query_latency(took.as_secs_f64() * 1000.0);
        // 事件可观测:成功发 Query(字段与实际操作一致),失败发 Error(FC-DEPLOY-INV-030)。
        match &result {
            Ok((hits, candidates)) => crate::core::observe::emit(
                self.config.observer.as_ref(),
                crate::core::observe::Event::Query {
                    took,
                    candidates: *candidates,
                    returned: hits.len(),
                    channels: u8::from(self.vector.is_some()) + u8::from(self.text.is_some()),
                },
            ),
            Err(error) => crate::core::observe::emit(
                self.config.observer.as_ref(),
                crate::core::observe::Event::Error {
                    kind: crate::core::observe::ErrorKind::of(error),
                    context: "query::execute",
                },
            ),
        }
        result.map(|(hits, _candidates)| hits)
    }

    /// `execute` 的实际流水线(延迟采样包裹在外层)。
    fn execute_inner(&self) -> Result<(Vec<Hit>, usize)> {
        let view = self.prepare_view()?;
        self.validate_query()?;
        self.validate_fusion()?;
        self.validate_diversify()?;
        self.validate_dedup()?;
        let Some(ns_id) = self.resolve_ns_id(&view) else {
            return Ok((Vec::new(), 0));
        };
        // 历史视图(`as_of`)的 TTL 判定以视图时刻为准(设计 07 §25):
        // 记录在 `as_of(t)` 中可见当且仅当 `expires_at > t`,与墙上时钟无关。
        let now = self
            .as_of
            .unwrap_or_else(|| self.config.clock.now_unix_ms());
        let view = self.apply_as_of(view);
        let (scored, candidate_count) = self.run_channels(&view, ns_id, now)?;
        let (scored, via_map) = self.apply_expansion(&view, ns_id, scored, now);
        let ranked = self.rank(&view, scored, now);
        let hits = self.build_hits(&view, ranked, self.resolve_query_id(), &via_map);
        // 读路径命中计入访问统计:仅当前视图检索(历史/快照检索不污染当前统计),
        // 且仅在有持久层或自动遗忘时报数(设计 07 §2;热路径一次内存追加)。
        if self.pinned.is_none() && self.as_of.is_none() && self.table.tracks_access_hits() {
            self.table.record_hits(hits.iter().map(|hit| hit.rowid));
        }
        Ok((self.apply_rerank(hits), candidate_count))
    }

    /// 取检索视图:优先钉住的快照,否则取当前读视图;校验关闭态与通道非空。
    fn prepare_view(&self) -> Result<Arc<ReaderView>> {
        let view = match &self.pinned {
            Some(view) => Arc::clone(view),
            None => self.table.view(),
        };
        if view.closed {
            return Err(MnemeError::Closed);
        }
        if self.vector.is_none() && self.text.is_none() {
            return Err(MnemeError::Config {
                reason: "检索至少需要一个查询通道",
            });
        }
        Ok(view)
    }

    /// 校验查询向量维度与 `top_k`/`ef` 上限。
    fn validate_query(&self) -> Result<()> {
        if let Some(query) = &self.vector {
            let expected = self.config.dimension.get() as usize;
            if query.len() != expected {
                return Err(MnemeError::DimensionMismatch {
                    expected: self.config.dimension.get(),
                    got: query.len(),
                });
            }
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

    /// 校验融合配置:融合需要双通道(`Fusion` 单独设置即拒绝,绝不静默忽略);
    /// `Weighted.alpha` 须在 `[0,1]` 内且为有限值(NaN 被区间判定拒绝)。
    fn validate_fusion(&self) -> Result<()> {
        let dual = self.vector.is_some() && self.text.is_some();
        if self.fusion.is_some() && !dual {
            return Err(MnemeError::Config {
                reason: "Fusion 需要向量与文本两个通道",
            });
        }
        if let Some(Fusion::Weighted { alpha }) = self.fusion
            && !(0.0..=1.0).contains(&alpha)
        {
            return Err(MnemeError::Config {
                reason: "Fusion::Weighted.alpha 必须是 [0,1] 内的有限值",
            });
        }
        Ok(())
    }

    /// 校验多样性策略参数:非有限 `lambda` 会让 MMR 的 `clamp` 对 NaN 失效并静默退化
    /// (固定取首项),故入口显式拒绝(FC-MEM-PRE-003,拒绝静默失败)。
    fn validate_diversify(&self) -> Result<()> {
        if let Diversity::Mmr { lambda } = self.diversify
            && !lambda.is_finite()
        {
            return Err(MnemeError::Config {
                reason: "MMR lambda 必须是 [0,1] 内的有限值",
            });
        }
        Ok(())
    }

    /// 校验结果级去重参数:`Near` 阈值须为 `[0,1]` 内的有限值——`NaN`/越界会让
    /// `cosine_sim >= threshold` 恒假、去重静默空转,入口显式拒绝
    /// (FC-SCORE-POST-005,拒绝静默失败)。
    fn validate_dedup(&self) -> Result<()> {
        if let ResultDedup::Near { threshold } = self.dedup
            && !(0.0..=1.0).contains(&threshold)
        {
            return Err(MnemeError::Config {
                reason: "ResultDedup::Near.threshold 必须是 [0,1] 内的有限值",
            });
        }
        Ok(())
    }

    /// 解析命名空间路径对应的 `NsId`(未注册则 `None`)。
    fn resolve_ns_id(&self, view: &ReaderView) -> Option<NsId> {
        view.ns_by_path.get(&*self.ns_path).copied()
    }

    /// 指定 `as_of` 时在版本链上重建历史视图。
    fn apply_as_of(&self, view: Arc<ReaderView>) -> Arc<ReaderView> {
        match self.as_of {
            Some(ts_ms) => Arc::new(temporal::snapshot_at(&view, ts_ms)),
            None => view,
        }
    }

    /// 过滤先行 + 通道执行 + 融合。
    ///
    /// 计划器做块级下推(zone map/bloom),两通道共享同一份候选位图(过滤先行,
    /// 与融合顺序无关,I6);双通道各取 `2k` 再融合取 `k`,单通道直接取 `k`。
    fn run_channels(
        &self,
        view: &ReaderView,
        ns_id: NsId,
        now: i64,
    ) -> Result<(Vec<Scored>, usize)> {
        let plan = plan::compile(view, ns_id, self.filter.as_ref(), now);
        let candidate_count = plan.candidates.len();
        // 不变量:选择性是候选/活行的比值,必落在 [0,1]。
        debug_assert!((0.0..=1.0).contains(&plan.selectivity));
        let candidates = &plan.candidates;
        let dual = self.vector.is_some() && self.text.is_some();
        let channel_k = if dual {
            self.top_k.saturating_mul(2)
        } else {
            self.top_k
        };
        let channel = ChannelCtx {
            view,
            ns_id,
            now,
            top_k: channel_k,
            candidates,
        };
        let vector_hits = match &self.vector {
            Some(query) => Some(self.run_vector(&channel, query)?),
            None => None,
        };
        let text_hits = self.text.as_deref().map(|text| {
            bm25::search(&Bm25Query {
                view: channel.view,
                ns_id: channel.ns_id,
                query: text,
                top_k: channel.top_k,
                now_ms: channel.now,
                stopwords: self.config.tuning.stopwords,
                candidates: Some(&plan.bits),
            })
        });
        let fused = self.fuse_channels(vector_hits, text_hits)?;
        Ok((fused, candidate_count))
    }

    /// 融合两通道结果:单通道直取;双通道按 `fusion` 策略融合(缺省 RRF)。
    fn fuse_channels(
        &self,
        vector_hits: Option<Vec<Scored>>,
        text_hits: Option<Vec<Scored>>,
    ) -> Result<Vec<Scored>> {
        match (vector_hits, text_hits) {
            (Some(vector), Some(text)) => Ok(fusion::fuse(
                vector,
                text,
                self.fusion.unwrap_or_default(),
                fusion::FusionParams {
                    top_k: self.top_k,
                    vector_is_distance: self.config.metric == Metric::Euclidean,
                },
            )),
            (Some(vector), None) => Ok(vector),
            (None, Some(text)) => Ok(text),
            (None, None) => Err(MnemeError::Config {
                reason: "检索至少需要一个查询通道",
            }),
        }
    }

    /// 向量通道(带共享候选位图)。
    ///
    /// `Scoring` 开启任一非相似度因子时把 ANN 探查宽度放大到
    /// `ef' = max(ef, 4·top_k)`,让"相似度略低但综合分高"的记忆进入候选池;
    /// 默认 `Scoring`(仅相似度)与未开启时不放大,排序与纯相似度路径全等
    /// (FC-SCORE-POST-003)。
    fn run_vector(&self, ctx: &ChannelCtx<'_>, query: &[f32]) -> Result<Vec<Scored>> {
        let ef = self.ef.unwrap_or(self.config.hnsw.ef_search as usize);
        let ef = if self.scoring_needs_oversample() {
            ef.max(ctx.top_k.saturating_mul(4))
        } else {
            ef
        };
        let bias_holder;
        let bias = match self.scoring.as_ref() {
            Some(scoring) if scoring.bias_routing => {
                bias_holder = ScoringBias {
                    view: ctx.view,
                    c_norm: (scoring.c_norm.max(1)) as f32,
                };
                Some(&bias_holder as &dyn crate::memory::index::NodeBias)
            }
            _ => None,
        };
        search::search(&search::SearchParams {
            view: ctx.view,
            ns_id: ctx.ns_id,
            query,
            metric: self.config.metric,
            top_k: ctx.top_k,
            ef,
            filter: self.filter.as_ref(),
            now_ms: ctx.now,
            block: self.config.tuning.parallel_block,
            parallelism: self.config.parallelism,
            brute_force_max_rows: self.config.tuning.brute_force_max_rows as usize,
            filter_post_threshold: self.config.tuning.filter_post_threshold,
            filter_brute_threshold: self.config.tuning.filter_brute_threshold,
            rescore_oversample: self.config.tuning.rescore_oversample,
            candidates: Some(ctx.candidates),
            bias,
        })
    }

    /// `Scoring` 是否开启了任一非相似度因子(决定候选放大与偏置路由的必要性)。
    fn scoring_needs_oversample(&self) -> bool {
        self.scoring.as_ref().is_some_and(|scoring| {
            scoring.w_recency != 0.0
                || scoring.w_importance != 0.0
                || scoring.w_access != 0.0
                || scoring.w_confidence != 0.0
                || scoring.floor > 0.0
        })
    }

    /// 生成查询幂等标识。
    fn resolve_query_id(&self) -> QueryId {
        self.query_id
            .unwrap_or_else(|| QueryId(NEXT_QUERY_ID.fetch_add(1, Ordering::Relaxed)))
    }

    /// 沿关系边做联想扩展,返回扩展后的候选与来源边映射。
    ///
    /// 扩展只在本命名空间内推进,跨命名空间边视为不存在(`FC-SCORE-POST-004`)。
    /// 与既有通道候选按 `RowId` 合并,分数取 `max(自身分, boost)`,同一记录绝不
    /// 重复出现;`via` 只在扩展确有贡献(新增候选或提升分数)时记录
    /// (`FC-SCORE-POST-006`)。
    fn apply_expansion(
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
        (scored, via_map)
    }

    /// 综合打分重排(未开启 `Scoring` 时退化为纯通道分)。
    fn rank(
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

/// `Scoring::bias_routing` 的遍历偏置:重要度 + 归一化访问频次。
///
/// 只被 [`crate::memory::search`] 传给 HNSW 前沿优先级,**不参与最终打分**
/// (`FC-SCORE-POST-007`);`c_norm` 由调用方夹到 ≥1 防除零。
struct ScoringBias<'a> {
    view: &'a ReaderView,
    c_norm: f32,
}

impl crate::memory::index::NodeBias for ScoringBias<'_> {
    fn bias(&self, slot: crate::core::types::SlotId) -> f32 {
        let Some(slot_data) = self.view.slots.get(slot.get() as usize) else {
            return 0.0;
        };
        let access = self
            .view
            .access
            .get(&slot_data.rowid)
            .copied()
            .unwrap_or_default();
        slot_data.importance + (access.access_count as f32 / self.c_norm).min(1.0)
    }
}
