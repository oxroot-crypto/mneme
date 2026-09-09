//! `Namespace` 访问强化与检索反馈(`namespace/access.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::options::{Feedback, QueryId, RelationKind};
use crate::core::types::{Key, RowId};
use crate::memory::mutate_helpers::{lower_confidence, touch_rowid};
use crate::memory::relation::{self, Edge};

use super::Namespace;

impl Namespace {
    /// 访问强化:计数 +1、刷新最近访问;`boost=Some(d)` 同时提升 importance。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// assert!(ns.touch("a", Some(0.1)).unwrap());
    /// ```
    pub fn touch(&self, key: &str, boost: Option<f32>) -> Result<bool> {
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        let Some(ns_id) = ws.ns_id_of(&self.ns_path) else {
            return Ok(false);
        };
        let Some(rowid) = ws.key_index.get(&(ns_id, Key::new(key))).copied() else {
            return Ok(false);
        };
        let now = self.config.clock.now_unix_ms();
        let hit = touch_rowid(&mut ws, rowid, boost, now)?;
        self.table.publish(&ws);
        Ok(hit)
    }

    /// 按 `RowId` 访问强化。
    pub fn touch_by_rowid(&self, id: RowId, boost: Option<f32>) -> Result<bool> {
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        let now = self.config.clock.now_unix_ms();
        let hit = touch_rowid(&mut ws, id, boost, now)?;
        self.table.publish(&ws);
        Ok(hit)
    }

    /// 检索反馈闭环(幂等键 `(rowid, query_id)`,不变量 I27)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Feedback, InsertOutcome, Mneme, QueryId, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// let rowid = match ns.insert(Record::new(vec![1.0, 0.0])).unwrap() {
    ///     InsertOutcome::Inserted(id) => id,
    ///     other => panic!("unexpected: {other:?}"),
    /// };
    /// assert!(ns.feedback(rowid, Feedback::Used, QueryId(1)).unwrap());
    /// assert!(!ns.feedback(rowid, Feedback::Used, QueryId(1)).unwrap());
    /// ```
    pub fn feedback(&self, id: RowId, feedback: Feedback, query_id: QueryId) -> Result<bool> {
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        if !ws.feedback_seen.insert((id, query_id.0)) {
            return Ok(false);
        }
        let now = self.config.clock.now_unix_ms();
        match feedback {
            Feedback::Used => {
                touch_rowid(&mut ws, id, Some(0.02), now)?;
            }
            Feedback::Ignored => {}
            Feedback::Corrected { by } => {
                let edge = Edge {
                    from: by,
                    to: id,
                    kind: RelationKind::CONTRADICTS,
                    weight: 1.0,
                    metadata: Meta::Null,
                };
                relation::upsert_edge(Arc::make_mut(&mut ws.out_edges), edge.clone());
                relation::upsert_edge(Arc::make_mut(&mut ws.in_edges), edge);
                lower_confidence(&mut ws, id, now)?;
            }
        }
        self.table.publish(&ws);
        Ok(true)
    }
}
