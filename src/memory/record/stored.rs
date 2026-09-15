//! 点读结果的 owned 快照 `StoredRecord`。

use crate::core::meta::Meta;
use crate::core::types::RowId;

use super::view::RecordRef;
use super::write::Record;

/// 点读结果的 owned 快照(跨线程 / async 门面用;借读见 [`RecordRef`])。
///
/// 携带 `RowId` 与全部字段的独立副本,可安全跨线程移动。与 [`Record`] 的分工:
/// `Record` 是**写入前**类型(无 `RowId`),本类型是**读出后**的只读快照。
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRecord {
    rowid: RowId,
    key: Option<String>,
    text: Option<String>,
    vector: Vec<f32>,
    metadata: Meta,
    expires_at: Option<i64>,
    importance: f32,
    confidence: f32,
    valid_from: i64,
    valid_to: Option<i64>,
    provenance: Option<Meta>,
}

impl StoredRecord {
    /// 由借用视图复制为 owned 快照(内部点读路径与 async 门面用)。
    pub(crate) fn from_ref(record: &RecordRef<'_>) -> Self {
        Self {
            rowid: record.rowid(),
            key: record.key().map(str::to_owned),
            text: record.text().map(str::to_owned),
            vector: record.vector().to_vec(),
            metadata: record.metadata().clone(),
            expires_at: record.expires_at(),
            importance: record.importance(),
            confidence: record.confidence(),
            valid_from: record.valid_from(),
            valid_to: record.valid_to(),
            provenance: record.provenance().cloned(),
        }
    }

    /// 全局稳定逻辑标识。
    pub fn rowid(&self) -> RowId {
        self.rowid
    }

    /// 外部键;未设置返回 `None`。
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// 文本;未设置返回 `None`。
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// 向量切片。
    pub fn vector(&self) -> &[f32] {
        &self.vector
    }

    /// 元数据(未设置时为默认空元数据)。
    pub fn metadata(&self) -> &Meta {
        &self.metadata
    }

    /// 过期时刻(Unix 毫秒);无 TTL 返回 `None`。
    pub fn expires_at(&self) -> Option<i64> {
        self.expires_at
    }

    /// 重要度。
    pub fn importance(&self) -> f32 {
        self.importance
    }

    /// 可信度。
    pub fn confidence(&self) -> f32 {
        self.confidence
    }

    /// 有效时间起(Unix 毫秒)。
    pub fn valid_from(&self) -> i64 {
        self.valid_from
    }

    /// 有效时间止(Unix 毫秒);未设置返回 `None`。
    pub fn valid_to(&self) -> Option<i64> {
        self.valid_to
    }

    /// 来源/派生链;未设置返回 `None`。
    pub fn provenance(&self) -> Option<&Meta> {
        self.provenance.as_ref()
    }

    /// 转回可写 [`Record`](crate::Record)(便于改字段后重新写入)。
    ///
    /// `Record` 无 `RowId` 字段,`RowId` 不随转换保留;绝对过期时刻亦因
    /// `Record` 只接受相对 TTL 而丢弃(`ttl = None`)。
    ///
    /// # Returns
    /// 去掉 [`RowId`] 与绝对过期时刻的可写记录;其余字段逐项保留。
    ///
    /// # Examples
    ///
    /// ```
    /// use mneme::{Mneme, Record};
    ///
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// let stored = ns.get("a").unwrap().unwrap().to_stored();
    /// let record = stored.into_record();
    /// // 转回的可写记录可再次写入,向量内容逐位保留(去掉 RowId 与绝对 TTL)。
    /// ns.insert(record).unwrap();
    /// assert_eq!(ns.get("a").unwrap().unwrap().vector(), &[1.0, 0.0]);
    /// ```
    pub fn into_record(self) -> Record {
        Record {
            vector: self.vector,
            key: self.key,
            text: self.text,
            metadata: Some(self.metadata),
            ttl: None,
            importance: Some(self.importance),
            valid_from: Some(self.valid_from),
            valid_to: self.valid_to,
            confidence: Some(self.confidence),
            provenance: self.provenance,
        }
    }
}
