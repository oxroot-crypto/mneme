//! `Namespace` 关系边操作(`namespace/relation.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::options::RelationKind;
use crate::core::types::RowId;
use crate::memory::relation::{self, Edge};

use super::Namespace;
impl Namespace {
    /// 建立/更新一条关系边(`(from,to,kind)` 幂等)。
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
    /// assert_eq!(ns.neighbors(from, &[]).unwrap().len(), 1);
    /// ```
    pub fn relate(&self, from: RowId, to: RowId, kind: RelationKind, weight: f32) -> Result<()> {
        self.relate_with_meta(from, to, kind, weight, Meta::Null)
    }

    /// 建立/更新一条关系边并写入边元数据。
    pub fn relate_with_meta(
        &self,
        from: RowId,
        to: RowId,
        kind: RelationKind,
        weight: f32,
        metadata: Meta,
    ) -> Result<()> {
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        let edge = Edge {
            from,
            to,
            kind,
            weight: weight.clamp(0.0, 1.0),
            metadata,
        };
        relation::upsert_edge(Arc::make_mut(&mut ws.out_edges), edge.clone());
        relation::upsert_edge(Arc::make_mut(&mut ws.in_edges), edge);
        self.table.publish(&ws);
        Ok(())
    }

    /// 删除一条关系边,返回是否命中。
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
    /// assert!(ns.unrelate(from, to, RelationKind::RELATED).unwrap());
    /// ```
    pub fn unrelate(&self, from: RowId, to: RowId, kind: RelationKind) -> Result<bool> {
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        let removed = relation::remove_edge(Arc::make_mut(&mut ws.out_edges), from, to, kind);
        relation::remove_edge(Arc::make_mut(&mut ws.in_edges), to, from, kind);
        self.table.publish(&ws);
        Ok(removed)
    }

    /// 返回 `from` 的出边(两端存活,不变量 I25)。
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
    /// assert_eq!(ns.neighbors(from, &[RelationKind::RELATED]).unwrap().len(), 1);
    /// ```
    pub fn neighbors(&self, from: RowId, kinds: &[RelationKind]) -> Result<Vec<Edge>> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        Ok(view
            .out_edges
            .get(&from)
            .map(|edges| {
                edges
                    .iter()
                    .filter(|edge| {
                        (kinds.is_empty() || kinds.contains(&edge.kind))
                            && view.live_slot(edge.from).is_some()
                            && view.live_slot(edge.to).is_some()
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    /// 返回指向 `to` 的入边(两端存活)。
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
    /// assert_eq!(ns.predecessors(to, &[]).unwrap().len(), 1);
    /// ```
    pub fn predecessors(&self, to: RowId, kinds: &[RelationKind]) -> Result<Vec<Edge>> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let mut result = Vec::new();
        let buckets: Vec<&Vec<Edge>> = if let Some(edges) = view.in_edges.get(&to) {
            vec![edges]
        } else {
            view.out_edges.values().collect()
        };
        for edges in buckets {
            for edge in edges {
                if edge.to == to
                    && (kinds.is_empty() || kinds.contains(&edge.kind))
                    && view.live_slot(edge.from).is_some()
                    && view.live_slot(edge.to).is_some()
                {
                    result.push(edge.clone());
                }
            }
        }
        Ok(result)
    }
}
