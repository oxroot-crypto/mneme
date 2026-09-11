//! 快照句柄与快照上的只读命名空间视图(`snapshot.rs`)。

use std::marker::PhantomData;
use std::sync::Arc;

use crate::core::error::Result;
use crate::core::options::Diversity;
use crate::core::types::{Key, NsId, RowId};
use crate::memory::dedup::ResultDedup;
use crate::memory::namespace::{DEFAULT_TOP_K, point_get};
use crate::memory::pred::{self, EvalCtx, Expr};
use crate::memory::record::RecordRef;
use crate::memory::search_builder::SearchBuilder;
use crate::memory::table::{ReaderView, SlotData, Table};

/// 快照句柄:钉住某个读视图。
#[derive(Clone)]
pub struct SnapshotHandle {
    pub(crate) table: Arc<Table>,
    pub(crate) view: Arc<ReaderView>,
    pub(crate) as_of_ms: i64,
}

impl std::fmt::Debug for SnapshotHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotHandle")
            .field("version", &self.view.seqno)
            .field("as_of_ms", &self.as_of_ms)
            .finish_non_exhaustive()
    }
}

impl SnapshotHandle {
    /// 构建该视图时的基线序号水位。
    pub fn version(&self) -> u64 {
        self.view.seqno.get()
    }

    /// 事务时间上界(普通快照 = 当前,`as_of` = 指定时刻)。
    pub fn as_of_ms(&self) -> i64 {
        self.as_of_ms
    }

    /// 在钉住的快照上取命名空间只读视图。
    ///
    /// # Arguments
    /// * `path` - 命名空间路径,按 `/` 分层;不校验是否已注册。
    ///
    /// # Returns
    /// 指向 `path` 的 [`SnapshotNamespace`];不校验路径是否已注册。
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
    pub fn namespace(&self, path: &str) -> SnapshotNamespace {
        SnapshotNamespace {
            table: Arc::clone(&self.table),
            view: Arc::clone(&self.view),
            ns_path: Arc::from(path),
            as_of_ms: self.as_of_ms,
        }
    }
}

/// 快照上的命名空间只读视图。
#[derive(Clone)]
pub struct SnapshotNamespace {
    pub(crate) table: Arc<Table>,
    pub(crate) view: Arc<ReaderView>,
    pub(crate) ns_path: Arc<str>,
    /// 快照时刻:检索的 TTL 可见性判定以它为准(设计 07 §25)。
    pub(crate) as_of_ms: i64,
}

impl std::fmt::Debug for SnapshotNamespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotNamespace")
            .field("ns_path", &self.ns_path)
            .field("as_of_ms", &self.as_of_ms)
            .finish_non_exhaustive()
    }
}

impl SnapshotNamespace {
    pub(crate) fn ns_id(&self) -> Option<NsId> {
        self.view.ns_registry.iter().find_map(|(id, path)| {
            if **path == *self.ns_path {
                Some(*id)
            } else {
                None
            }
        })
    }

    /// 在钉住的视图上查询。
    ///
    /// # Returns
    /// 在钉住视图上配置检索参数的 [`SearchBuilder`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.namespace("demo")
    ///     .insert(Record::new(vec![1.0, 0.0]))
    ///     .unwrap();
    /// let snap = db.snapshot().namespace("demo");
    /// let hits = snap.search().vector(&[1.0, 0.0]).top_k(1).execute().unwrap();
    /// assert_eq!(hits.len(), 1);
    /// ```
    pub fn search(&self) -> SearchBuilder<'_> {
        SearchBuilder {
            table: Arc::clone(&self.table),
            config: Arc::clone(&self.table.config),
            ns_path: Arc::clone(&self.ns_path),
            pinned: Some(Arc::clone(&self.view)),
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
            as_of: Some(self.as_of_ms),
            query_id: None,
            rerank: None,
            _marker: PhantomData,
        }
    }

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

    /// 统计命中活记录数。
    ///
    /// # Arguments
    /// * `filter` - 三值过滤表达式;`None` 表示不过滤。
    ///
    /// # Returns
    /// 命中过滤条件的活记录数;命名空间未注册返回 `0`。逻辑过期以**快照时刻
    /// `as_of_ms`** 判定(FC-QUERY-POST-006)。
    ///
    /// # Errors
    /// 当前恒 `Ok`(视图被钉住、不探测关闭态;`Result` 为 L2 持久层错误预留)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.namespace("demo")
    ///     .insert(Record::new(vec![1.0, 0.0]))
    ///     .unwrap();
    /// let snap = db.snapshot().namespace("demo");
    /// assert_eq!(snap.count(None).unwrap(), 1);
    /// ```
    pub fn count(&self, filter: Option<Expr>) -> Result<u64> {
        let Some(ns_id) = self.ns_id() else {
            return Ok(0);
        };
        let now = self.as_of_ms;
        let mut count = 0;
        for (idx, slot) in self.view.slots.iter().enumerate() {
            if self.view.dead.get(idx) || slot.ns_id != ns_id || !slot.is_live(now) {
                continue;
            }
            if !passes_filter(&filter, slot, &self.view, &slot.rowid) {
                continue;
            }
            count += 1;
        }
        Ok(count)
    }
}

/// 判断记录是否通过可选过滤表达式(三值语义,缺失字段不命中)。
pub(crate) fn passes_filter(
    filter: &Option<Expr>,
    slot_data: &SlotData,
    view: &ReaderView,
    rowid: &RowId,
) -> bool {
    match filter {
        Some(expr) => pred::matches(
            expr,
            &EvalCtx {
                slot: slot_data,
                access: view.access.get(rowid).copied(),
            },
        ),
        None => true,
    }
}
