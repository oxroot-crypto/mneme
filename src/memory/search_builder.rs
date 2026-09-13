//! 检索构建器(`search_builder.rs`)。
//!
//! 承载 `SearchBuilder` 的链式配置与字段;公开入口 `execute()` 与执行流水线
//! (视图准备/双通道扫描/融合/扩展/排序/去重)见 L4 的
//! [`query::exec`](crate::query)。公开签名在 L1 冻结(设计 03 §2.2)。

use std::marker::PhantomData;
use std::sync::Arc;

use crate::core::options::{Diversity, QueryId, Scoring};
use crate::memory::config::Config;
use crate::memory::dedup::ResultDedup;
use crate::memory::pred::Expr;
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
    /// 双通道融合器;`None` = 使用默认 RRF(仅双通道时生效)。
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

    /// 设置查询文本(BM25 关键词通道;可与 `vector` 组合做 RRF 融合)。
    ///
    /// # Arguments
    ///
    /// * `query` - 查询文本;分词后按命名空间全局统计打分(设计 06 §3)。
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

    /// 设置双通道融合器(默认 RRF `k=60`;仅双通道时生效)。
    ///
    /// # Arguments
    ///
    /// * `fusion` - RRF 或加权融合;未同时设置向量与文本通道时
    ///   `execute()` 返回 `Config`,绝不静默忽略。
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::core::options::Feedback;
    use crate::core::types::RowId;
    use crate::memory::engine::Mneme;
    use crate::memory::record::{Hit, Record};
    use crate::memory::rerank::QueryCtx;

    #[test]
    fn query_id_is_carried_into_hits_and_feedback_idempotency() {
        let db = Mneme::in_memory(2).expect("in_memory");
        let ns = db.namespace("t");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("insert");
        let hits = ns
            .search()
            .vector(&[1.0, 0.0])
            .top_k(1)
            .query_id(QueryId(42))
            .execute()
            .expect("search");
        assert_eq!(
            hits[0].query_id,
            QueryId(42),
            "构建器指定的幂等标识原样回传"
        );
        assert!(
            ns.feedback(hits[0].rowid, Feedback::Used, QueryId(42))
                .expect("feedback")
        );
        assert!(
            !ns.feedback(hits[0].rowid, Feedback::Used, QueryId(42))
                .expect("feedback"),
            "同一 (rowid, query_id) 只记一次"
        );
    }

    struct ReversingReranker {
        calls: AtomicUsize,
    }

    impl Reranker for ReversingReranker {
        fn rerank(&self, _ctx: &QueryCtx<'_>, mut hits: Vec<Hit>) -> Vec<Hit> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            hits.reverse();
            hits
        }
    }

    #[test]
    fn rerank_hook_is_applied_once_without_changing_hit_set() {
        let db = Mneme::in_memory(2).expect("in_memory");
        let ns = db.namespace("t");
        for key in ["a", "b", "c"] {
            ns.insert(Record::new(vec![1.0, 0.0]).key(key))
                .expect("insert");
        }
        let reranker = Arc::new(ReversingReranker {
            calls: AtomicUsize::new(0),
        });
        let hits = ns
            .search()
            .vector(&[1.0, 0.0])
            .top_k(3)
            .rerank(reranker.clone())
            .execute()
            .expect("search");
        assert_eq!(reranker.calls.load(Ordering::Relaxed), 1, "钩子调用一次");
        assert_eq!(hits.len(), 3, "命中集合不变");
        let mut rowids: Vec<RowId> = hits.iter().map(|hit| hit.rowid).collect();
        rowids.sort();
        rowids.dedup();
        assert_eq!(rowids.len(), 3, "三个不同命中");
        assert!(
            hits.windows(2).all(|pair| pair[0].rowid > pair[1].rowid),
            "钩子反转了同分命中的顺序"
        );
    }
}
