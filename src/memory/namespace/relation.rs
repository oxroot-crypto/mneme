//! `Namespace` 关系边操作(`namespace/relation.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::options::RelationKind;
use crate::core::types::RowId;
use crate::memory::relation::{self, Edge, RelateOptions};

use super::Namespace;
impl Namespace {
    /// 建立/更新一条关系边(`(from,to,kind)` 幂等)。
    ///
    /// # Arguments
    /// * `from` - 出边源 `RowId`。
    /// * `to` - 出边目标 `RowId`。
    /// * `kind` - 关系类型。
    /// * `weight` - 边权重;钳制到 `[0.0, 1.0]`。
    ///
    /// # Returns
    /// 恒 `Ok`;幂等键为 `(from, to, kind)`,重复 relate 覆盖 weight(metadata 不变)。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
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
        self.relate_with_options(from, to, RelateOptions::new(kind, weight))
    }

    /// 建立/更新一条关系边并写入边元数据;参数经 [`RelateOptions`] 打包
    /// (幂等键仍为 `(from, to, kind)`,重复 relate 为 upsert)。
    ///
    /// # Arguments
    /// * `from` - 出边源 `RowId`。
    /// * `to` - 出边目标 `RowId`。
    /// * `options` - 关系参数(类型、权重、边元数据);`weight` 钳制到 `[0.0, 1.0]`。
    ///
    /// # Returns
    /// 恒 `Ok`。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    pub fn relate_with_options(
        &self,
        from: RowId,
        to: RowId,
        options: RelateOptions,
    ) -> Result<()> {
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        let edge = Edge {
            from,
            to,
            kind: options.kind,
            weight: options.weight.clamp(0.0, 1.0),
            metadata: options.metadata,
        };
        relation::upsert_edge(Arc::make_mut(&mut ws.out_edges), edge.clone());
        relation::upsert_edge(Arc::make_mut(&mut ws.in_edges), edge);
        self.table.publish(&ws);
        Ok(())
    }

    /// 删除一条关系边,返回是否命中。
    ///
    /// # Arguments
    /// * `from` - 出边源 `RowId`。
    /// * `to` - 出边目标 `RowId`。
    /// * `kind` - 关系类型;须与建立时相同才可删除。
    ///
    /// # Returns
    /// 命中并移除出边时返回 `true`(入边同步移除);不存在该边返回 `false`。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
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
    /// # Arguments
    /// * `from` - 出边源 `RowId`。
    /// * `kinds` - 关系类型过滤;空切片表示不过滤。
    ///
    /// # Returns
    /// 过滤 `kinds` 且两端仍存活的出边列表;悬挂边不可见。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
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
    /// # Arguments
    /// * `to` - 入边目标 `RowId`。
    /// * `kinds` - 关系类型过滤;空切片表示不过滤。
    ///
    /// # Returns
    /// 过滤 `kinds` 且 `edge.to == to`、两端存活的边列表。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
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
