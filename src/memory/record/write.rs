//! 写侧值类型:待写入记录与写入/更新结果。

use std::time::Duration;

use crate::core::meta::Meta;
use crate::core::types::RowId;

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
    /// # Arguments
    ///
    /// * `vector` - 初始向量;分量须为有限值,维度须与建库维度一致。
    ///
    /// # Returns
    ///
    /// 待写入记录;除向量外的字段均未设置,可经链式 setter 补齐。
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
    ///
    /// # Arguments
    ///
    /// * `key` - 应用层的记忆 id;长度限额在 `insert` 入口校验。
    ///
    /// # Returns
    ///
    /// 携带外部键的记录(链式)。
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// 设置文本(可选;启用文本去重与 BM25 检索)。
    ///
    /// # Arguments
    ///
    /// * `text` - 记忆正文;长度限额在 `insert` 入口校验。
    ///
    /// # Returns
    ///
    /// 携带文本的记录(链式)。
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// 设置元数据(开放 JSON)。
    ///
    /// # Arguments
    ///
    /// * `metadata` - 任意 JSON 值;大小/深度限额在 `insert` 入口校验。
    ///
    /// # Returns
    ///
    /// 携带元数据的记录(链式)。
    pub fn metadata(mut self, metadata: Meta) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// 设置 TTL;缺省永不过期。
    ///
    /// # Arguments
    ///
    /// * `ttl` - 存活时长;过期时刻 = 写入时刻 + TTL。
    ///
    /// # Returns
    ///
    /// 携带 TTL 的记录(链式)。
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// 设置重要度(越界钳制到 `[0,1]`,缺省 0.5;非有限值(NaN)在 `insert` 入口拒绝)。
    ///
    /// # Arguments
    ///
    /// * `importance` - 重要度,`[0,1]`。
    ///
    /// # Returns
    ///
    /// 携带重要度的记录(链式)。
    pub fn importance(mut self, importance: f32) -> Self {
        self.importance = Some(importance);
        self
    }

    /// 设置有效时间起(缺省 = `created_at`)。
    ///
    /// # Arguments
    ///
    /// * `ts_ms` - 有效时间起点(Unix 毫秒)。
    ///
    /// # Returns
    ///
    /// 携带有效时间起的记录(链式)。
    pub fn valid_from(mut self, ts_ms: i64) -> Self {
        self.valid_from = Some(ts_ms);
        self
    }

    /// 设置有效时间止(开区间)。
    ///
    /// # Arguments
    ///
    /// * `ts_ms` - 有效时间终点(Unix 毫秒,开区间)。
    ///
    /// # Returns
    ///
    /// 携带有效时间止的记录(链式)。
    pub fn valid_to(mut self, ts_ms: i64) -> Self {
        self.valid_to = Some(ts_ms);
        self
    }

    /// 设置可信度(越界钳制到 `[0,1]`,缺省 1.0;非有限值(NaN)在 `insert` 入口拒绝)。
    ///
    /// # Arguments
    ///
    /// * `confidence` - 可信度,`[0,1]`。
    ///
    /// # Returns
    ///
    /// 携带可信度的记录(链式)。
    pub fn confidence(mut self, confidence: f32) -> Self {
        self.confidence = Some(confidence);
        self
    }

    /// 设置来源/派生链。
    ///
    /// # Arguments
    ///
    /// * `provenance` - 任意 JSON 值,描述来源与派生关系。
    ///
    /// # Returns
    ///
    /// 携带来源/派生链的记录(链式)。
    pub fn provenance(mut self, provenance: Meta) -> Self {
        self.provenance = Some(provenance);
        self
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
