//! 快照句柄与快照上的只读命名空间视图(`snapshot.rs`)。

use std::marker::PhantomData;
use std::sync::Arc;

use crate::core::error::Result;
use crate::core::options::{Diversity, RelationKind};
use crate::core::types::{Key, NsId, RowId};
use crate::memory::dedup::ResultDedup;
use crate::memory::namespace::{DEFAULT_TOP_K, point_get};
use crate::memory::pred::{self, EvalCtx, Expr};
use crate::memory::record::RecordRef;
use crate::memory::relation::Edge;
use crate::memory::search_builder::{Fusion, SearchBuilder};
use crate::memory::table::{ReaderView, Table};

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
        }
    }
}

/// 快照上的命名空间只读视图。
#[derive(Clone)]
pub struct SnapshotNamespace {
    table: Arc<Table>,
    view: Arc<ReaderView>,
    ns_path: Arc<str>,
}

impl SnapshotNamespace {
    fn ns_id(&self) -> Option<NsId> {
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
            fusion: Fusion::default(),
            scoring: None,
            diversify: Diversity::Off,
            expand: None,
            as_of: None,
            query_id: None,
            rerank: None,
            _marker: PhantomData,
        }
    }

    /// 按 key 点读。
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
        let now = self.table.config.clock.now_unix_ms();
        Ok(self
            .ns_id()
            .and_then(|ns_id| point_get(&self.view, ns_id, &Key::new(key), now)))
    }

    /// 按 `RowId` 点读。
    pub fn get_by_rowid(&self, id: RowId) -> Result<Option<RecordRef<'_>>> {
        let now = self.table.config.clock.now_unix_ms();
        Ok(self
            .view
            .live_slot(id)
            .map(|slot| Arc::clone(&self.view.slots[slot.get() as usize]))
            .filter(|slot_data| slot_data.is_live(now))
            .map(RecordRef::new))
    }

    /// 批量点读。
    pub fn get_many(&self, keys: &[&str]) -> Result<Vec<Option<RecordRef<'_>>>> {
        let now = self.table.config.clock.now_unix_ms();
        let ns_id = self.ns_id();
        Ok(keys
            .iter()
            .map(|key| ns_id.and_then(|ns_id| point_get(&self.view, ns_id, &Key::new(*key), now)))
            .collect())
    }

    /// 单独取回原始向量。
    pub fn get_vector(&self, id: RowId) -> Result<Option<Vec<f32>>> {
        let now = self.table.config.clock.now_unix_ms();
        Ok(self
            .view
            .live_slot(id)
            .map(|slot| &self.view.slots[slot.get() as usize])
            .filter(|slot_data| slot_data.is_live(now))
            .map(|slot_data| slot_data.vector.to_vec()))
    }

    /// 存在性判定。
    pub fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }

    /// 统计命中活记录数。
    pub fn count(&self, filter: Option<Expr>) -> Result<u64> {
        let Some(ns_id) = self.ns_id() else {
            return Ok(0);
        };
        let now = self.table.config.clock.now_unix_ms();
        let mut count = 0;
        for (idx, slot) in self.view.slots.iter().enumerate() {
            if self.view.dead.get(idx) || slot.ns_id != ns_id || !slot.is_live(now) {
                continue;
            }
            if let Some(expr) = &filter {
                let ctx = EvalCtx {
                    slot,
                    access: self.view.access.get(&slot.rowid).copied(),
                };
                if !pred::matches(expr, &ctx) {
                    continue;
                }
            }
            count += 1;
        }
        Ok(count)
    }

    /// 出边(两端存活)。
    pub fn neighbors(&self, from: RowId, kinds: &[RelationKind]) -> Result<Vec<Edge>> {
        Ok(self
            .view
            .out_edges
            .get(&from)
            .map(|edges| {
                edges
                    .iter()
                    .filter(|edge| {
                        (kinds.is_empty() || kinds.contains(&edge.kind))
                            && self.view.live_slot(edge.from).is_some()
                            && self.view.live_slot(edge.to).is_some()
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    /// 入边(两端存活)。
    pub fn predecessors(&self, to: RowId, kinds: &[RelationKind]) -> Result<Vec<Edge>> {
        Ok(self
            .view
            .out_edges
            .values()
            .flatten()
            .filter(|edge| {
                edge.to == to
                    && (kinds.is_empty() || kinds.contains(&edge.kind))
                    && self.view.live_slot(edge.from).is_some()
                    && self.view.live_slot(edge.to).is_some()
            })
            .cloned()
            .collect())
    }

    /// 流式遍历(不含墓碑/过期记录)。
    pub fn iter(
        &self,
        filter: Option<Expr>,
    ) -> Result<impl Iterator<Item = Result<RecordRef<'_>>> + '_> {
        self.iter_with(filter, false)
    }

    /// 流式遍历;`include_deleted=true` 时包含墓碑/过期记录。
    pub fn iter_with(
        &self,
        filter: Option<Expr>,
        include_deleted: bool,
    ) -> Result<impl Iterator<Item = Result<RecordRef<'_>>> + '_> {
        let now = self.table.config.clock.now_unix_ms();
        let mut collected = Vec::new();
        if let Some(ns_id) = self.ns_id() {
            for (rowid, slot) in self.view.latest.iter() {
                let slot_data = &self.view.slots[slot.get() as usize];
                if slot_data.ns_id != ns_id {
                    continue;
                }
                if !include_deleted
                    && (self.view.dead.get(slot.get() as usize) || !slot_data.is_live(now))
                {
                    continue;
                }
                if let Some(expr) = &filter {
                    let ctx = EvalCtx {
                        slot: slot_data,
                        access: self.view.access.get(rowid).copied(),
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
