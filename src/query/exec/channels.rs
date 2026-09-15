//! 过滤先行 + 双通道执行与融合,含 ANN 偏置路由(设计 06 §5)。

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::types::NsId;
use crate::memory::search::{self, Scored};
use crate::memory::search_builder::SearchBuilder;
use crate::memory::table::ReaderView;
use crate::query::bm25::{self, Bm25Query};
use crate::query::{fusion, plan};

/// 单通道执行的公共上下文(视图/命名空间/时刻/条数/共享候选)。
struct ChannelCtx<'a> {
    view: &'a ReaderView,
    ns_id: NsId,
    now: i64,
    top_k: usize,
    candidates: &'a [u32],
}

impl SearchBuilder<'_> {
    /// 过滤先行 + 通道执行 + 融合。
    ///
    /// 计划器做块级下推(zone map/bloom),两通道共享同一份候选位图(过滤先行,
    /// 与融合顺序无关,I6);双通道各取 `2k` 再融合取 `k`,单通道直接取 `k`。
    pub(super) fn run_channels(
        &self,
        view: &ReaderView,
        ns_id: NsId,
        now: i64,
    ) -> Result<(Vec<Scored>, usize)> {
        let plan = plan::compile(view, ns_id, self.filter.as_ref(), now);
        let candidate_count = plan.candidates().len();
        // 不变量:选择性是候选/活行的比值,必落在 [0,1]。
        debug_assert!((0.0..=1.0).contains(&plan.selectivity));
        let candidates = plan.candidates();
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
                candidates: Some(plan.bits()),
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
