//! `Namespace` 读路径与检索(`namespace/query.rs`)。

use std::marker::PhantomData;
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::options::Diversity;
use crate::core::types::{Key, NsId, RowId};
use crate::memory::dedup::ResultDedup;
use crate::memory::pred::{self, EvalCtx, Expr};
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

    /// 统计命中活记录数(不物化记录)。
    ///
    /// # Arguments
    /// * `filter` - 三值过滤表达式;`None` 表示不过滤。
    ///
    /// # Returns
    /// 命中过滤条件的活记录数;命名空间未注册返回 `0`。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0])).unwrap();
    /// ns.insert(Record::new(vec![0.0, 1.0])).unwrap();
    /// assert_eq!(ns.count(None).unwrap(), 2);
    /// ```
    pub fn count(&self, filter: Option<Expr>) -> Result<u64> {
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
        let Some(ns_id) = ns_id else {
            return Ok(0);
        };
        let mut count = 0;
        for (idx, slot) in view.slots.iter().enumerate() {
            if view.dead.get(idx) || slot.ns_id != ns_id || !slot.is_live(now) {
                continue;
            }
            if let Some(expr) = &filter {
                let ctx = EvalCtx {
                    slot,
                    access: view.access.get(&slot.rowid).copied(),
                };
                if !pred::matches(expr, &ctx) {
                    continue;
                }
            }
            count += 1;
        }
        Ok(count)
    }

    /// 按过滤条件遍历(不含墓碑/过期记录),调用前物化命中行的 `Arc` 句柄、
    /// 不复制记录体(FC-MEM-CPLX-005,非流式)。
    ///
    /// # Arguments
    /// * `filter` - 三值过滤表达式;`None` 表示不过滤。
    ///
    /// # Returns
    /// 按 `RowId` 升序产出 `Ok(RecordRef)` 的迭代器(内层 `Err` 为 L2 预留)。
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
    /// let keys: Vec<String> = ns
    ///     .iter(None)
    ///     .unwrap()
    ///     .map(|record| record.unwrap().key().unwrap().to_string())
    ///     .collect();
    /// assert_eq!(keys, vec!["a"]);
    /// ```
    pub fn iter(
        &self,
        filter: Option<Expr>,
    ) -> Result<impl Iterator<Item = Result<RecordRef<'_>>> + '_> {
        self.iter_with(filter, false)
    }

    /// 按过滤条件遍历(导出/审计/重建用);`include_deleted=true` 时包含墓碑/过期记录(仅审计)。
    ///
    /// # Arguments
    /// * `filter` - 三值过滤表达式;`None` 表示不过滤。
    /// * `include_deleted` - `true` 时包含墓碑/已逻辑过期记录。
    ///
    /// # Returns
    /// 按 `RowId` 升序产出 `Ok(RecordRef)` 的迭代器(内层 `Err` 为 L2 预留)。
    /// 调用前物化命中行的 `Arc` 句柄、不复制记录体(FC-MEM-CPLX-005,非流式)。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    pub fn iter_with(
        &self,
        filter: Option<Expr>,
        include_deleted: bool,
    ) -> Result<impl Iterator<Item = Result<RecordRef<'_>>> + '_> {
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
        let mut collected = Vec::new();
        if let Some(ns_id) = ns_id {
            for (rowid, slot) in view.latest.iter() {
                let slot_data = &view.slots[slot.get() as usize];
                if slot_data.ns_id != ns_id {
                    continue;
                }
                if !include_deleted
                    && (view.dead.get(slot.get() as usize) || !slot_data.is_live(now))
                {
                    continue;
                }
                if let Some(expr) = &filter {
                    let ctx = EvalCtx {
                        slot: slot_data,
                        access: view.access.get(rowid).copied(),
                    };
                    if !pred::matches(expr, &ctx) {
                        continue;
                    }
                }
                collected.push(Arc::clone(slot_data));
            }
        }
        collected.sort_by_key(|slot_data| slot_data.rowid);
        Ok(collected
            .into_iter()
            .map(|slot_data| Ok(RecordRef::new(slot_data))))
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
