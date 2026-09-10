//! `Namespace` 读路径与检索(`namespace/query.rs`)。

use std::marker::PhantomData;
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::options::Diversity;
use crate::core::types::{Key, NsId, RowId};
use crate::memory::dedup::ResultDedup;
use crate::memory::record::RecordRef;
use crate::memory::search_builder::SearchBuilder;
use crate::memory::table::ReaderView;

use super::{DEFAULT_TOP_K, Namespace};
impl Namespace {
    /// 开始一次检索。
    ///
    /// # Returns
    /// 链式配置检索参数的 [`SearchBuilder`]。
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
    pub fn search(&self) -> SearchBuilder<'_> {
        SearchBuilder {
            table: Arc::clone(&self.table),
            config: Arc::clone(&self.config),
            ns_path: Arc::clone(&self.ns_path),
            pinned: None,
            vector: None,
            text: None,
            top_k: DEFAULT_TOP_K,
            ef: None,
            filter: None,
            dedup: ResultDedup::Off,
            fusion: None,
            scoring: None,
            diversify: Diversity::Off,
            expand: None,
            as_of: None,
            query_id: None,
            rerank: None,
            _marker: PhantomData,
        }
    }

    /// 按 key 点读(快照一致)。
    ///
    /// # Arguments
    /// * `key` - 记录键;按当前命名空间隔离查找。
    ///
    /// # Returns
    /// 命中时返回记录只读视图;命名空间未注册或 key 不存在返回 `None`。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// assert_eq!(ns.get("a").unwrap().unwrap().key(), Some("a"));
    /// ```
    pub fn get(&self, key: &str) -> Result<Option<RecordRef<'_>>> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let now = self.config.clock.now_unix_ms();
        let Some(ns_id) = view.ns_registry.iter().find_map(|(id, path)| {
            if **path == *self.ns_path {
                Some(*id)
            } else {
                None
            }
        }) else {
            return Ok(None);
        };
        Ok(point_get(&view, ns_id, &Key::new(key), now))
    }

    /// 按 `RowId` 点读。
    ///
    /// # Arguments
    /// * `id` - 目标 `RowId`;全库共享同一编号空间。
    ///
    /// # Returns
    /// 命中活记录时返回只读视图;`RowId` 不存在、已墓碑或已逻辑过期返回 `None`。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
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
    /// assert!(ns.get_by_rowid(rowid).unwrap().is_some());
    /// ```
    pub fn get_by_rowid(&self, id: RowId) -> Result<Option<RecordRef<'_>>> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let now = self.config.clock.now_unix_ms();
        Ok(view
            .live_slot(id)
            .map(|slot| Arc::clone(&view.slots[slot.get() as usize]))
            .filter(|slot_data| slot_data.is_live(now))
            .map(RecordRef::new))
    }

    /// 批量点读,返回顺序与输入一一对应。
    ///
    /// # Arguments
    /// * `keys` - 记录键列表;未命中的位置以 `None` 占位。
    ///
    /// # Returns
    /// 与 `keys` 等长、顺序一致的命中视图列表。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// let refs = ns.get_many(&["a", "missing"]).unwrap();
    /// assert!(refs[0].is_some());
    /// assert!(refs[1].is_none());
    /// ```
    pub fn get_many(&self, keys: &[&str]) -> Result<Vec<Option<RecordRef<'_>>>> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let now = self.config.clock.now_unix_ms();
        let ns_id = view.ns_registry.iter().find_map(|(id, path)| {
            if **path == *self.ns_path {
                Some(*id)
            } else {
                None
            }
        });
        Ok(keys
            .iter()
            .map(|key| ns_id.and_then(|ns_id| point_get(&view, ns_id, &Key::new(*key), now)))
            .collect())
    }

    /// 批量按 `RowId` 点读,返回顺序与输入一一对应。
    ///
    /// # Arguments
    /// * `ids` - `RowId` 列表;未命中的位置以 `None` 占位。
    ///
    /// # Returns
    /// 与 `ids` 等长、顺序一致的命中视图列表。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    pub fn get_many_by_rowid(&self, ids: &[RowId]) -> Result<Vec<Option<RecordRef<'_>>>> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let now = self.config.clock.now_unix_ms();
        Ok(ids
            .iter()
            .map(|id| {
                view.live_slot(*id)
                    .map(|slot| Arc::clone(&view.slots[slot.get() as usize]))
                    .filter(|slot_data| slot_data.is_live(now))
                    .map(RecordRef::new)
            })
            .collect())
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
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// assert!(ns.exists("a").unwrap());
    /// ```
    pub fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }

    /// 单独取回原始向量。
    ///
    /// # Arguments
    /// * `id` - 目标 `RowId`;仅活记录可见。
    ///
    /// # Returns
    /// 命中活记录时返回向量拷贝;不可见时返回 `None`。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
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
    /// assert_eq!(ns.get_vector(rowid).unwrap().unwrap(), vec![1.0, 0.0]);
    /// ```
    pub fn get_vector(&self, id: RowId) -> Result<Option<Vec<f32>>> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let now = self.config.clock.now_unix_ms();
        Ok(view
            .live_slot(id)
            .map(|slot| &view.slots[slot.get() as usize])
            .filter(|slot_data| slot_data.is_live(now))
            .map(|slot_data| slot_data.vector.to_vec()))
    }
}

pub(crate) fn point_get(
    view: &ReaderView,
    ns_id: NsId,
    key: &Key,
    now: i64,
) -> Option<RecordRef<'static>> {
    let rowid = view.rowid_of_key(ns_id, key)?;
    let slot = view.live_slot(rowid)?;
    let slot_data = &view.slots[slot.get() as usize];
    slot_data
        .is_live(now)
        .then(|| RecordRef::new(Arc::clone(slot_data)))
}
