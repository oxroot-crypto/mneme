//! 快照命名空间的遍历与关系读取(`snapshot_scan.rs`)。
//!
//! 以 `impl SnapshotNamespace` 扩展点读之外的扫描/关系方法;遍历非流式,
//! 调用前物化命中行的 `Arc` 句柄、不复制记录体(FC-MEM-CPLX-005)。

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::options::RelationKind;
use crate::core::types::RowId;
use crate::memory::pred::Expr;
use crate::memory::record::RecordRef;
use crate::memory::relation::Edge;
use crate::memory::snapshot::{SnapshotNamespace, passes_filter};

impl SnapshotNamespace {
    /// 出边(两端存活)。
    ///
    /// # Arguments
    /// * `from` - 出边源 `RowId`。
    /// * `kinds` - 关系类型过滤;空切片表示不过滤。
    ///
    /// # Returns
    /// 过滤 `kinds` 且两端仍存活的出边列表;悬挂边不可见。
    ///
    /// # Errors
    /// 当前恒 `Ok`(视图被钉住、不探测关闭态;`Result` 为 L2 持久层错误预留)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{InsertOutcome, Mneme, Record, RelationKind};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// let from = match ns.insert(Record::new(vec![1.0, 0.0])).unwrap() {
    ///     InsertOutcome::Inserted(id) => id,
    ///     other => panic!("unexpected: {other:?}"),
    /// };
    /// let to = match ns.insert(Record::new(vec![0.0, 1.0])).unwrap() {
    ///     InsertOutcome::Inserted(id) => id,
    ///     other => panic!("unexpected: {other:?}"),
    /// };
    /// ns.relate(from, to, RelationKind::RELATED, 0.5).unwrap();
    /// let snap = db.snapshot().namespace("demo");
    /// assert_eq!(snap.neighbors(from, &[]).unwrap().len(), 1);
    /// ```
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
    ///
    /// # Arguments
    /// * `to` - 入边目标 `RowId`。
    /// * `kinds` - 关系类型过滤;空切片表示不过滤。
    ///
    /// # Returns
    /// 过滤 `kinds` 且 `edge.to == to`、两端存活的边列表。
    ///
    /// # Errors
    /// 当前恒 `Ok`(视图被钉住、不探测关闭态;`Result` 为 L2 持久层错误预留)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{InsertOutcome, Mneme, Record, RelationKind};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// let from = match ns.insert(Record::new(vec![1.0, 0.0])).unwrap() {
    ///     InsertOutcome::Inserted(id) => id,
    ///     other => panic!("unexpected: {other:?}"),
    /// };
    /// let to = match ns.insert(Record::new(vec![0.0, 1.0])).unwrap() {
    ///     InsertOutcome::Inserted(id) => id,
    ///     other => panic!("unexpected: {other:?}"),
    /// };
    /// ns.relate(from, to, RelationKind::RELATED, 0.5).unwrap();
    /// let snap = db.snapshot().namespace("demo");
    /// assert_eq!(snap.predecessors(to, &[]).unwrap().len(), 1);
    /// ```
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

    /// 遍历(不含墓碑/过期记录),调用前物化命中行的 `Arc` 句柄、不复制记录体
    /// (FC-MEM-CPLX-005,非流式)。
    ///
    /// # Arguments
    /// * `filter` - 三值过滤表达式;`None` 表示不过滤。
    ///
    /// # Returns
    /// 按 `RowId` 升序产出 `Ok(RecordRef)` 的迭代器(内层 `Err` 为 L2 预留)。
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
    /// let keys: Vec<String> = snap
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

    /// 遍历(导出/审计/重建用);`include_deleted=true` 时包含墓碑/过期记录。
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
    /// 当前恒 `Ok`(视图被钉住、不探测关闭态;`Result` 为 L2 持久层错误预留)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// ns.delete("a").unwrap();
    /// let snap = db.snapshot().namespace("demo");
    /// assert_eq!(snap.iter(None).unwrap().count(), 0);
    /// assert_eq!(snap.iter_with(None, true).unwrap().count(), 1);
    /// ```
    pub fn iter_with(
        &self,
        filter: Option<Expr>,
        include_deleted: bool,
    ) -> Result<impl Iterator<Item = Result<RecordRef<'_>>> + '_> {
        // TTL 可见性以快照时刻为准,与 `SnapshotNamespace` 的其它读路径一致
        // (FC-QUERY-POST-006),绝不混用墙上时钟。
        let now = self.as_of_ms;
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
                if !passes_filter(&filter, slot_data, &self.view, rowid) {
                    continue;
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
