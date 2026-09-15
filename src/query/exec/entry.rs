//! 执行入口与可观测:延迟采样、事件发射与查询幂等标识(设计 06 §5)。

use std::sync::atomic::{AtomicU64, Ordering};

// `MnemeError` 只出现在 rustdoc 内链(`execute` 的 `# Errors`);crate 内无显式引用,放行警告。
#[allow(unused_imports)]
use crate::core::error::{MnemeError, Result};
use crate::core::options::QueryId;
use crate::memory::record::Hit;
use crate::memory::search_builder::SearchBuilder;

/// 全局查询标识分配器(`execute()` 缺省生成 `QueryId`)。
///
/// `Ordering::Relaxed` 足够:只要求同一计数器不重号,不承担跨线程可见性顺序;
/// `u64` 回绕需 2^64 次查询,按实际规模视为不可达。
static NEXT_QUERY_ID: AtomicU64 = AtomicU64::new(1);

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

    /// 生成查询幂等标识。
    fn resolve_query_id(&self) -> QueryId {
        self.query_id
            .unwrap_or_else(|| QueryId(NEXT_QUERY_ID.fetch_add(1, Ordering::Relaxed)))
    }
}
