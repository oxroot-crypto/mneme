//! 检索命中的物化视图 `Hit`。

use crate::core::meta::Meta;
use crate::core::options::QueryId;
use crate::core::types::{Key, RowId};
use crate::memory::relation::Edge;
use crate::memory::score::ScoreBreakdown;

/// 检索命中的物化视图(不含原始向量,需要时用 `get_vector`)。
#[derive(Debug, Clone)]
pub struct Hit {
    /// 全局稳定逻辑标识。
    pub rowid: RowId,
    /// 本次查询的幂等标识。
    pub query_id: QueryId,
    /// 外部键。
    pub key: Option<Key>,
    /// 最终分(默认相似度;开启 `Scoring` 后为综合分)。
    pub score: f32,
    /// 写入时刻(Unix 毫秒)。
    pub created_at: i64,
    /// 过期时刻。
    pub expires_at: Option<i64>,
    /// 重要度。
    pub importance: f32,
    /// 可信度。
    pub confidence: f32,
    /// 有效时间起。
    pub valid_from: i64,
    /// 有效时间止。
    pub valid_to: Option<i64>,
    /// 文本。
    pub text: Option<String>,
    /// 元数据。
    pub metadata: Meta,
    /// 来源/派生链。
    pub provenance: Option<Meta>,
    /// 由关系扩展命中时的来源边。
    pub via: Option<Edge>,
    /// 各打分因子贡献。
    pub(crate) breakdown: Option<ScoreBreakdown>,
}

impl Hit {
    /// 返回各打分因子贡献(调试/审计)。
    ///
    /// # Returns
    ///
    /// 本次命中的 [`ScoreBreakdown`];未开启
    /// [`Scoring`](crate::Scoring) 时仅 [`sim`](ScoreBreakdown::sim) 非零,
    /// 其余字段为 0,未携带打分明细时为全零默认值。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    ///
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// let hits = ns.search().vector(&[1.0, 0.0]).top_k(1).execute().unwrap();
    /// // 未开启 `Scoring`:明细中只有相似度分与最终分一致。
    /// assert_eq!(hits[0].explain().sim, hits[0].score);
    /// ```
    pub fn explain(&self) -> ScoreBreakdown {
        self.breakdown.unwrap_or_default()
    }
}
