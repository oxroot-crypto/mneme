//! 检索构建器(`search_builder.rs`)。
//!
//! 承载 `SearchBuilder` 的链式配置与公开入口 `execute()`;执行流水线
//! (视图准备/扫描/扩展/排序/去重)见 [`search_exec`](crate::memory::search_exec)。
//! 公开签名在 L1 冻结(设计 03 §2.2)。

use std::marker::PhantomData;
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::options::{Diversity, QueryId, Scoring};
use crate::memory::config::Config;
use crate::memory::dedup::ResultDedup;
use crate::memory::pred::Expr;
use crate::memory::record::Hit;
use crate::memory::relation::RelationExpand;
use crate::memory::rerank::{Fusion, Reranker};
use crate::memory::table::{ReaderView, Table};

/// 检索构建器。
///
/// 由 [`Namespace::search`](crate::memory::Namespace::search) 或
/// [`SnapshotNamespace::search`](crate::memory::SnapshotNamespace::search) 创建,
/// 链式设置参数后以 [`SearchBuilder::execute`] 执行。
///
/// # Examples
/// ```
/// use mneme::{Mneme, Record};
/// let db = Mneme::in_memory(2).unwrap();
/// let ns = db.namespace("demo");
/// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
/// let hits = ns
///     .search()
///     .vector(&[1.0, 0.0])
///     .top_k(1)
///     .execute()
///     .unwrap();
/// assert_eq!(hits.len(), 1);
/// ```
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
    /// 双通道融合器;`None` = 未设置(L1 未落地,设置即 `Unsupported`)。
    pub(crate) fusion: Option<Fusion>,
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
    ///
    /// # Arguments
    ///
    /// * `query` - 查询向量;长度须与建库维度一致。
    ///
    /// # Returns
    ///
    /// 携带查询向量的构建器(链式)。
    pub fn vector(mut self, query: &[f32]) -> Self {
        self.vector = Some(query.to_vec());
        self
    }

    /// 设置查询文本(L4 前 `execute()` 返回 `Unsupported`)。
    ///
    /// # Arguments
    ///
    /// * `query` - 查询文本;作为 BM25 通道(未实现)。
    ///
    /// # Returns
    ///
    /// 携带查询文本的构建器(链式)。
    pub fn text(mut self, query: &str) -> Self {
        self.text = Some(query.to_string());
        self
    }

    /// 设置返回条数(默认 10,上限 [`Limits::top_k_max`](crate::Limits::top_k_max))。
    ///
    /// # Arguments
    ///
    /// * `top_k` - 返回条数;`0` 表示不返回。
    ///
    /// # Returns
    ///
    /// 携带 `top_k` 的构建器(链式)。
    pub fn top_k(mut self, top_k: usize) -> Self {
        self.top_k = top_k;
        self
    }

    /// 设置探查宽度(仅 L3+ 生效;超上限返回 `LimitExceeded`)。
    ///
    /// # Arguments
    ///
    /// * `ef` - 探查宽度。
    ///
    /// # Returns
    ///
    /// 携带探查宽度的构建器(链式)。
    pub fn ef(mut self, ef: usize) -> Self {
        self.ef = Some(ef);
        self
    }

    /// 设置预过滤表达式。
    ///
    /// # Arguments
    ///
    /// * `filter` - 过滤 AST(三值求值,见设计 03 §5)。
    ///
    /// # Returns
    ///
    /// 携带过滤表达式的构建器(链式)。
    pub fn filter(mut self, filter: Expr) -> Self {
        self.filter = Some(filter);
        self
    }

    /// 设置结果级去重。
    ///
    /// # Arguments
    ///
    /// * `dedup` - 按 `RowId` 或近似相似度去重。
    ///
    /// # Returns
    ///
    /// 携带去重策略的构建器(链式)。
    pub fn dedup(mut self, dedup: ResultDedup) -> Self {
        self.dedup = dedup;
        self
    }

    /// 设置双通道融合器(L4 前 `execute()` 返回 `Unsupported`)。
    ///
    /// # Arguments
    ///
    /// * `fusion` - RRF 或加权融合;单独设置(无需 text 通道)即拒绝,绝不静默忽略。
    ///
    /// # Returns
    ///
    /// 携带融合器的构建器(链式)。
    pub fn fusion(mut self, fusion: Fusion) -> Self {
        self.fusion = Some(fusion);
        self
    }

    /// 开启综合打分。
    ///
    /// # Arguments
    ///
    /// * `scoring` - 综合打分配置(相似度/新鲜度/重要度等权重)。
    ///
    /// # Returns
    ///
    /// 携带打分配置的构建器(链式)。
    pub fn score(mut self, scoring: Scoring) -> Self {
        self.scoring = Some(scoring);
        self
    }

    /// 开启多样性。
    ///
    /// # Arguments
    ///
    /// * `diversify` - MMR 参数;`Off` = 关闭。
    ///
    /// # Returns
    ///
    /// 携带多样性策略的构建器(链式)。
    pub fn diversify(mut self, diversify: Diversity) -> Self {
        self.diversify = diversify;
        self
    }

    /// 开启关系联想扩展。
    ///
    /// # Arguments
    ///
    /// * `expand` - 跳数/关系类型/衰减/节点上限。
    ///
    /// # Returns
    ///
    /// 携带扩展参数的构建器(链式)。
    pub fn expand(mut self, expand: RelationExpand) -> Self {
        self.expand = Some(expand);
        self
    }

    /// 指定事务时间上界,做历史检索。
    ///
    /// # Arguments
    ///
    /// * `ts_ms` - 事务时间上界(Unix 毫秒)。
    ///
    /// # Returns
    ///
    /// 携带时间上界的构建器(链式)。
    pub fn as_of(mut self, ts_ms: i64) -> Self {
        self.as_of = Some(ts_ms);
        self
    }

    /// 指定查询幂等标识;缺省由 `execute()` 生成。
    ///
    /// # Arguments
    ///
    /// * `query_id` - 本次查询的幂等标识,反馈时原样回传。
    ///
    /// # Returns
    ///
    /// 携带查询标识的构建器(链式)。
    pub fn query_id(mut self, query_id: QueryId) -> Self {
        self.query_id = Some(query_id);
        self
    }

    /// 设置精排钩子。
    ///
    /// # Arguments
    ///
    /// * `rerank` - 精排钩子;不得改变命中集合语义。
    ///
    /// # Returns
    ///
    /// 携带精排钩子的构建器(链式)。
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
    /// * MMR `lambda` 含非有限值 → [`MnemeError::Config`](`clamp` 对 NaN 失效会静默退化);
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
        self.validate_diversify()?;
        let Some(ns_id) = self.resolve_ns_id(&view) else {
            return Ok(Vec::new());
        };
        let now = self.config.clock.now_unix_ms();
        let view = self.apply_as_of(view);
        let scored = self.run_search(&view, ns_id, query, now)?;
        let (scored, via_map) = self.apply_expansion(&view, scored, now);
        let ranked = self.rank(&view, scored, now);
        let hits = self.build_hits(&view, ranked, self.resolve_query_id(), &via_map);
        Ok(self.apply_rerank(hits))
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
