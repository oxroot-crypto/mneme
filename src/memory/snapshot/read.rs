//! 快照视图的按 key / `RowId` 点读。

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::types::{Key, RowId};
use crate::memory::namespace::point_get;
use crate::memory::record::RecordRef;

use super::namespace::SnapshotNamespace;

impl SnapshotNamespace {
    /// 按 key 点读。
    ///
    /// # Arguments
    /// * `key` - 记录键;按当前命名空间隔离查找。
    ///
    /// # Returns
    /// 命中时返回记录只读视图;命名空间未注册或 key 不存在返回 `None`。
    /// 可见性(墓碑/逻辑过期)以**快照时刻 `as_of_ms`** 判定(FC-QUERY-POST-006)。
    ///
    /// # Errors
    /// 当前恒 `Ok`(视图被钉住、不探测关闭态;`Result` 为 L2 持久层错误预留)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// let snap = db.snapshot().namespace("demo");
    /// assert!(snap.get("a").unwrap().is_some());
    /// ```
    pub fn get(&self, key: &str) -> Result<Option<RecordRef<'_>>> {
        let now = self.as_of_ms;
        Ok(self
            .ns_id()
            .and_then(|ns_id| point_get(&self.view, ns_id, &Key::new(key), now)))
    }

    /// 按 `RowId` 点读。
    ///
    /// # Arguments
    /// * `id` - 目标 `RowId`;全库共享同一编号空间。
    ///
    /// # Returns
    /// 命中活记录时返回只读视图;`RowId` 不存在、已墓碑或已逻辑过期返回 `None`。
    /// 过期以**快照时刻 `as_of_ms`** 判定,与墙上时钟无关。
    ///
    /// # Errors
    /// 当前恒 `Ok`(视图被钉住、不探测关闭态;`Result` 为 L2 持久层错误预留)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{InsertOutcome, Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// let rowid = match ns.insert(Record::new(vec![1.0, 0.0])).unwrap() {
    ///     InsertOutcome::Inserted(id) => id,
    ///     other => panic!("unexpected: {other:?}"),
    /// };
    /// let snap = db.snapshot().namespace("demo");
    /// assert!(snap.get_by_rowid(rowid).unwrap().is_some());
    /// ```
    pub fn get_by_rowid(&self, id: RowId) -> Result<Option<RecordRef<'_>>> {
        let now = self.as_of_ms;
        Ok(self
            .view
            .live_slot(id)
            .map(|slot| Arc::clone(&self.view.slots[slot.get() as usize]))
            .filter(|slot_data| slot_data.is_live(now))
            .map(RecordRef::new))
    }

    /// 批量点读。
    ///
    /// # Arguments
    /// * `keys` - 记录键列表;未命中的位置以 `None` 占位。
    ///
    /// # Returns
    /// 与 `keys` 等长、顺序一致的命中视图列表。可见性以快照时刻判定
    /// (同 [`get`](Self::get),FC-QUERY-POST-006)。
    ///
    /// # Errors
    /// 当前恒 `Ok`(视图被钉住、不探测关闭态;`Result` 为 L2 持久层错误预留)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// let snap = db.snapshot().namespace("demo");
    /// let refs = snap.get_many(&["a", "missing"]).unwrap();
    /// assert!(refs[0].is_some());
    /// assert!(refs[1].is_none());
    /// ```
    pub fn get_many(&self, keys: &[&str]) -> Result<Vec<Option<RecordRef<'_>>>> {
        let now = self.as_of_ms;
        let ns_id = self.ns_id();
        Ok(keys
            .iter()
            .map(|key| ns_id.and_then(|ns_id| point_get(&self.view, ns_id, &Key::new(*key), now)))
            .collect())
    }

    /// 按 `RowId` 批量点读。
    ///
    /// # Arguments
    /// * `ids` - `RowId` 列表;未命中的位置以 `None` 占位。
    ///
    /// # Returns
    /// 与 `ids` 等长、顺序一致的命中视图列表;可见性以快照时刻 `as_of_ms`
    /// 判定(同 [`get_by_rowid`](Self::get_by_rowid))。
    ///
    /// # Errors
    /// 当前恒 `Ok`(视图被钉住、不探测关闭态;`Result` 为 L2 持久层错误预留)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{InsertOutcome, Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// let rowid = match ns.insert(Record::new(vec![1.0, 0.0])).unwrap() {
    ///     InsertOutcome::Inserted(id) => id,
    ///     other => panic!("unexpected: {other:?}"),
    /// };
    /// let snap = db.snapshot().namespace("demo");
    /// let refs = snap.get_many_by_rowid(&[rowid]).unwrap();
    /// assert!(refs[0].is_some());
    /// ```
    pub fn get_many_by_rowid(&self, ids: &[RowId]) -> Result<Vec<Option<RecordRef<'_>>>> {
        let now = self.as_of_ms;
        Ok(ids
            .iter()
            .map(|id| {
                self.view
                    .live_slot(*id)
                    .map(|slot| Arc::clone(&self.view.slots[slot.get() as usize]))
                    .filter(|slot_data| slot_data.is_live(now))
                    .map(RecordRef::new)
            })
            .collect())
    }

    /// 单独取回原始向量。
    ///
    /// # Arguments
    /// * `id` - 目标 `RowId`;仅活记录可见。
    ///
    /// # Returns
    /// 命中活记录时返回向量拷贝;不可见时返回 `None`。可见性以快照时刻判定
    /// (同 [`get`](Self::get),FC-QUERY-POST-006)。
    ///
    /// # Errors
    /// 当前恒 `Ok`(视图被钉住、不探测关闭态;`Result` 为 L2 持久层错误预留)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{InsertOutcome, Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// let rowid = match ns.insert(Record::new(vec![1.0, 0.0])).unwrap() {
    ///     InsertOutcome::Inserted(id) => id,
    ///     other => panic!("unexpected: {other:?}"),
    /// };
    /// let snap = db.snapshot().namespace("demo");
    /// assert_eq!(snap.get_vector(rowid).unwrap().unwrap(), vec![1.0, 0.0]);
    /// ```
    pub fn get_vector(&self, id: RowId) -> Result<Option<Vec<f32>>> {
        let now = self.as_of_ms;
        Ok(self
            .view
            .live_slot(id)
            .map(|slot| &self.view.slots[slot.get() as usize])
            .filter(|slot_data| slot_data.is_live(now))
            .map(|slot_data| slot_data.vector.to_vec()))
    }

    /// 存在性判定。
    ///
    /// # Arguments
    /// * `key` - 记录键;按当前命名空间隔离查找。
    ///
    /// # Returns
    /// 存在活记录时返回 `true`。
    ///
    /// # Errors
    /// 当前恒 `Ok`(视图被钉住、不探测关闭态;`Result` 为 L2 持久层错误预留)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.namespace("demo")
    ///     .insert(Record::new(vec![1.0, 0.0]).key("a"))
    ///     .unwrap();
    /// let snap = db.snapshot().namespace("demo");
    /// assert!(snap.exists("a").unwrap());
    /// ```
    pub fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }
}
