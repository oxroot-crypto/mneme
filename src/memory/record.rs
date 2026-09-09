//! 记录值类型:待写入记录、只读视图与写入/更新结果(`record.rs`)。
//!
//! 这些类型是公开 API 的数据载体(设计 16 §1.2),不含存储实现。

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use crate::core::meta::Meta;
use crate::core::options::QueryId;
use crate::core::types::{Key, RowId};
use crate::memory::relation::Edge;
use crate::memory::score::ScoreBreakdown;
use crate::memory::table::SlotData;
/// 一条待写入的记忆。
///
/// 字段对 crate 内可见(`pub(crate)`),对外经链式 setter 构造;
/// `insert` 时统一校验维度与有限性(设计 16 §1.2)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    pub(crate) vector: Vec<f32>,
    pub(crate) key: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) metadata: Option<Meta>,
    pub(crate) ttl: Option<Duration>,
    pub(crate) importance: Option<f32>,
    pub(crate) valid_from: Option<i64>,
    pub(crate) valid_to: Option<i64>,
    pub(crate) confidence: Option<f32>,
    pub(crate) provenance: Option<Meta>,
}

impl Record {
    /// 以向量构造记录;维度在 `insert` 时校验,故本函数不返回 `Result`。
    ///
    /// # Examples
    /// ```
    /// use mneme::Record;
    /// let record = Record::new(vec![1.0, 0.0]).key("a").importance(0.9);
    /// ```
    pub fn new(vector: Vec<f32>) -> Self {
        Self {
            vector,
            ..Self::default()
        }
    }

    /// 设置外部键(可选;同 key 行为见 [`InsertMode`](crate::InsertMode))。
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// 设置文本(可选;启用文本去重与后续 BM25)。
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// 设置元数据(开放 JSON)。
    pub fn metadata(mut self, metadata: Meta) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// 设置 TTL;缺省永不过期。
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// 设置重要度(越界钳制到 `[0,1]`,缺省 0.5)。
    pub fn importance(mut self, importance: f32) -> Self {
        self.importance = Some(importance);
        self
    }

    /// 设置有效时间起(缺省 = `created_at`)。
    pub fn valid_from(mut self, ts_ms: i64) -> Self {
        self.valid_from = Some(ts_ms);
        self
    }

    /// 设置有效时间止(开区间)。
    pub fn valid_to(mut self, ts_ms: i64) -> Self {
        self.valid_to = Some(ts_ms);
        self
    }

    /// 设置可信度(越界钳制到 `[0,1]`,缺省 1.0)。
    pub fn confidence(mut self, confidence: f32) -> Self {
        self.confidence = Some(confidence);
        self
    }

    /// 设置来源/派生链。
    pub fn provenance(mut self, provenance: Meta) -> Self {
        self.provenance = Some(provenance);
        self
    }
}

/// 存储记录的只读视图(`get`/`iter` 等点读路径)。
///
/// 以 `Arc` 持有底层物理版本,故 `get() -> RecordRef<'_>` 在安全 Rust 下成立;
/// `key`/`text`/`vector` 访问器零拷贝(仅 `Arc` 引用计数)。
pub struct RecordRef<'a> {
    slot_data: Arc<SlotData>,
    _marker: PhantomData<&'a ()>,
}

impl std::fmt::Debug for RecordRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordRef")
            .field("rowid", &self.slot_data.rowid)
            .field("key", &self.slot_data.key)
            .finish_non_exhaustive()
    }
}

impl<'a> RecordRef<'a> {
    pub(crate) fn new(slot_data: Arc<SlotData>) -> Self {
        Self {
            slot_data,
            _marker: PhantomData,
        }
    }

    /// 全局稳定逻辑标识。
    pub fn rowid(&self) -> RowId {
        self.slot_data.rowid
    }

    /// 外部键。
    pub fn key(&self) -> Option<&str> {
        self.slot_data.key.as_ref().map(Key::as_str)
    }

    /// 写入时刻(Unix 毫秒)。
    pub fn created_at(&self) -> i64 {
        self.slot_data.created_at
    }

    /// 过期时刻;`None` = 永不过期。
    pub fn expires_at(&self) -> Option<i64> {
        self.slot_data.expires_at
    }

    /// 重要度。
    pub fn importance(&self) -> f32 {
        self.slot_data.importance
    }

    /// 文本。
    pub fn text(&self) -> Option<&str> {
        self.slot_data.text.as_deref()
    }

    /// 元数据。
    pub fn metadata(&self) -> &Meta {
        &self.slot_data.meta
    }

    /// 有效时间起。
    pub fn valid_from(&self) -> i64 {
        self.slot_data.valid_from
    }

    /// 有效时间止。
    pub fn valid_to(&self) -> Option<i64> {
        self.slot_data.valid_to
    }

    /// 可信度。
    pub fn confidence(&self) -> f32 {
        self.slot_data.confidence
    }

    /// 来源/派生链。
    pub fn provenance(&self) -> Option<&Meta> {
        self.slot_data.provenance.as_ref()
    }

    /// 原始向量(零拷贝)。
    pub fn vector(&self) -> &[f32] {
        &self.slot_data.vector
    }

    /// 克隆为可写 [`Record`](去重 `Merge` 回调等使用)。
    pub fn to_record(&self) -> Record {
        Record {
            vector: self.slot_data.vector.to_vec(),
            key: self
                .slot_data
                .key
                .as_ref()
                .map(|key| key.as_str().to_string()),
            text: self.slot_data.text.as_ref().map(|text| text.to_string()),
            metadata: Some(self.slot_data.meta.clone()),
            ttl: None,
            importance: Some(self.slot_data.importance),
            valid_from: Some(self.slot_data.valid_from),
            valid_to: self.slot_data.valid_to,
            confidence: Some(self.slot_data.confidence),
            provenance: self.slot_data.provenance.clone(),
        }
    }
}

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
    pub fn explain(&self) -> ScoreBreakdown {
        self.breakdown.unwrap_or_default()
    }
}

/// 写入结果。
#[derive(Debug, Clone, PartialEq)]
pub enum InsertOutcome {
    /// 新建。
    Inserted(RowId),
    /// 去重合并,保留旧 `RowId`。
    Merged(RowId),
    /// 去重拒绝。
    Duplicate {
        /// 命中的既有记录。
        existing: RowId,
        /// 相似度分。
        score: f32,
    },
}

/// 更新结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// 更新成功。
    Updated(RowId),
    /// 目标不存在。
    NotFound,
}
