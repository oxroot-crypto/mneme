//! 存储记录的只读借用视图 `RecordRef`。

use std::marker::PhantomData;
use std::sync::Arc;

use crate::core::meta::Meta;
use crate::core::types::{Key, RowId};
use crate::memory::table::SlotData;

use super::stored::StoredRecord;
use super::write::Record;

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
    ///
    /// # Returns
    ///
    /// 本条物理版本对应的 [`RowId`]。
    pub fn rowid(&self) -> RowId {
        self.slot_data.rowid
    }

    /// 外部键。
    ///
    /// # Returns
    ///
    /// 记录的外部键;未设置 key 时返回 `None`。
    pub fn key(&self) -> Option<&str> {
        self.slot_data.key.as_ref().map(Key::as_str)
    }

    /// 写入时刻(Unix 毫秒)。
    ///
    /// # Returns
    ///
    /// 本条物理版本的写入时刻(Unix 毫秒)。
    pub fn created_at(&self) -> i64 {
        self.slot_data.created_at
    }

    /// 过期时刻;`None` = 永不过期。
    ///
    /// # Returns
    ///
    /// 过期时刻(Unix 毫秒,开区间);未设置 TTL 时返回 `None`。
    pub fn expires_at(&self) -> Option<i64> {
        self.slot_data.expires_at
    }

    /// 重要度。
    ///
    /// # Returns
    ///
    /// 重要度,`[0,1]`;写入时未显式设置则为缺省 0.5。
    pub fn importance(&self) -> f32 {
        self.slot_data.importance
    }

    /// 文本。
    ///
    /// # Returns
    ///
    /// 记忆正文;未设置文本时返回 `None`。
    pub fn text(&self) -> Option<&str> {
        self.slot_data.text.as_deref()
    }

    /// 元数据。
    ///
    /// # Returns
    ///
    /// 元数据;未设置时为 [`Meta::Null`]。
    pub fn metadata(&self) -> &Meta {
        &self.slot_data.meta
    }

    /// 有效时间起。
    ///
    /// # Returns
    ///
    /// 有效时间起点(Unix 毫秒);写入时未显式设置则等于
    /// [`created_at`](Self::created_at)。
    pub fn valid_from(&self) -> i64 {
        self.slot_data.valid_from
    }

    /// 有效时间止。
    ///
    /// # Returns
    ///
    /// 有效时间终点(Unix 毫秒,开区间);未设置时返回 `None`。
    pub fn valid_to(&self) -> Option<i64> {
        self.slot_data.valid_to
    }

    /// 可信度。
    ///
    /// # Returns
    ///
    /// 可信度,`[0,1]`;写入时未显式设置则为缺省 1.0。
    pub fn confidence(&self) -> f32 {
        self.slot_data.confidence
    }

    /// 来源/派生链。
    ///
    /// # Returns
    ///
    /// 来源/派生链元数据;未设置时返回 `None`。
    pub fn provenance(&self) -> Option<&Meta> {
        self.slot_data.provenance.as_ref()
    }

    /// 原始向量(零拷贝)。
    ///
    /// # Returns
    ///
    /// 本条物理版本的向量切片;零拷贝借用本视图,生命周期随 `RecordRef`。
    pub fn vector(&self) -> &[f32] {
        &self.slot_data.vector
    }

    /// 克隆为可写 [`Record`](去重 `Merge` 回调等使用)。
    ///
    /// # Returns
    ///
    /// 等价于当前只读视图内容的新记录;`ttl` 置 `None`(过期语义随写入重算)。
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

    /// 克隆为 owned 快照 [`StoredRecord`](含 `RowId`,可跨线程移动)。
    ///
    /// # Returns
    ///
    /// 与当前只读视图同字段的独立副本;async 门面与需要跨线程携带记录的场景使用。
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
    /// assert_eq!(stored.key(), Some("a"));
    /// assert_eq!(stored.vector(), &[1.0, 0.0]);
    /// ```
    pub fn to_stored(&self) -> StoredRecord {
        StoredRecord::from_ref(self)
    }
}
