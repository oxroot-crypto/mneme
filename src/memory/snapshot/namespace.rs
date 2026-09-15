//! 快照上的命名空间只读视图 [`SnapshotNamespace`] 与检索入口。

use std::marker::PhantomData;
use std::sync::Arc;

use crate::core::options::Diversity;
use crate::core::types::NsId;
use crate::memory::dedup::ResultDedup;
use crate::memory::namespace::DEFAULT_TOP_K;
use crate::memory::search_builder::SearchBuilder;
use crate::memory::table::{ReaderView, Table};

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
}
