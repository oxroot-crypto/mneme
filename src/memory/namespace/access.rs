//! `Namespace` 访问强化与检索反馈(`namespace/access.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::options::{Feedback, QueryId, RelationKind};
use crate::core::types::{Key, RowId};
use crate::memory::mutate_helpers::{lower_confidence, touch_rowid};
use crate::memory::relation::Edge;
use crate::memory::write_helpers::latest_live;

use super::Namespace;

/// `Feedback::Used` 对重要度的提升量(轻度强化;来源:设计 10 §4,δ_up 默认 +0.02)。
const FEEDBACK_USED_IMPORTANCE_BOOST: f32 = 0.02;

impl Namespace {
    /// 访问强化:计数 +1、刷新最近访问;`boost=Some(d)` 同时提升 importance。
    ///
    /// # Arguments
    /// * `key` - 记录键;按当前命名空间隔离查找。
    /// * `boost` - 重要度增量;`Some(d)` 时 `importance += d` 后钳制到 `[0.0, 1.0]`;
    ///   `d` 含非有限值(NaN)→ `NonFinite`。
    ///
    /// # Returns
    /// 命中可见记录(未墓碑、未逻辑过期)返回 `true`(计数已更新);命名空间未注册、
    /// key 不存在或记录不可见返回 `false`(FC-MEM-POST-009)。
    ///
    /// # Errors
    /// 库已关闭 → [`MnemeError::Closed`];`boost` 含非有限值 → [`MnemeError::NonFinite`]。
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
        let config = Arc::clone(&self.config);
        let ns_path = Arc::clone(&self.ns_path);
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            let Some(ns_id) = ws.ns_id_of(&ns_path) else {
                return Ok(false);
            };
            let Some(rowid) = ws.key_index.get(&(ns_id, Key::new(key))).copied() else {
                return Ok(false);
            };
            touch_rowid(ws, rowid, boost, config.clock.now_unix_ms())
        })
    }

    /// 按 `RowId` 访问强化。
    ///
    /// # Arguments
    /// * `id` - 目标 `RowId`;不存在或已墓碑返回 `false`。
    /// * `boost` - 重要度增量;语义同 [`Namespace::touch`]。
    ///
    /// # Returns
    /// 命中可见记录返回 `true`;`RowId` 不存在、已墓碑或已逻辑过期返回 `false`
    /// (FC-MEM-POST-009)。
    ///
    /// # Errors
    /// 库已关闭 → [`MnemeError::Closed`];`boost` 含非有限值 → [`MnemeError::NonFinite`]。
    pub fn touch_by_rowid(&self, id: RowId, boost: Option<f32>) -> Result<bool> {
        let config = Arc::clone(&self.config);
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            touch_rowid(ws, id, boost, config.clock.now_unix_ms())
        })
    }

    /// 检索反馈闭环(幂等键 `(rowid, query_id)`,不变量 I27)。
    ///
    /// # Arguments
    /// * `id` - 被反馈的 `RowId`。
    /// * `feedback` - 反馈类型:`Used` 强化重要度、`Ignored` 不生效、
    ///   `Corrected { by }` 建立 `CONTRADICTS` 边并降低可信度。
    /// * `query_id` - 查询幂等标识;与 `id` 组成幂等键。
    ///
    /// # Returns
    /// 首次收到该 `(rowid, query_id)` 且记录可见时返回 `true` 并生效;
    /// 重复反馈或记录不可见(不存在/已墓碑/已过期)返回 `false`——
    /// 不可见时不占用幂等键,该 `RowId` 重新可见后首次反馈仍生效。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
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
        let config = Arc::clone(&self.config);
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            // 不可见记录(不存在/已墓碑/已过期)无法被强化:先校验命中,再占用幂等键
            // (FC-SCORE-INV-027);`latest_live` 只查墓碑,逻辑过期须经 `is_live(now)` 判断。
            let now = config.clock.now_unix_ms();
            if latest_live(ws, id).is_none_or(|base| !base.is_live(now)) {
                return Ok(false);
            }
            if ws.feedback_seen.contains(&(id, query_id.0)) {
                return Ok(false);
            }
            match feedback {
                Feedback::Used => {
                    touch_rowid(ws, id, Some(FEEDBACK_USED_IMPORTANCE_BOOST), now)?;
                }
                Feedback::Ignored => {}
                Feedback::Corrected { by } => {
                    // 先做可失败的可信度下调,再写边;失败由 `write_tx` 整体回滚。
                    lower_confidence(ws, id, now)?;
                    let edge = Edge {
                        from: by,
                        to: id,
                        kind: RelationKind::CONTRADICTS,
                        weight: 1.0,
                        metadata: Meta::Null,
                    };
                    ws.relate_edge(edge);
                }
            }
            // 生效成功后再登记幂等键:中途失败不占用键,调用方可重试(FC-SCORE-INV-027)。
            Arc::make_mut(&mut ws.feedback_seen).insert((id, query_id.0));
            Ok(true)
        })
    }
}
