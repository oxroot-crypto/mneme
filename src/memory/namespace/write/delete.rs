//! `Namespace` 按 key/`RowId` 删除(`namespace/write/delete.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::types::{Key, RowId};
use crate::memory::Namespace;

impl Namespace {
    /// 按 key 删除,返回是否命中活记录。
    ///
    /// # Arguments
    /// * `key` - 记录键;按当前命名空间隔离查找。
    ///
    /// # Returns
    /// 命中活记录并写入墓碑返回 `true`;命名空间未注册或 key 不存在返回 `false`。
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
    /// assert!(ns.delete("a").unwrap());
    /// assert!(!ns.exists("a").unwrap());
    /// ```
    pub fn delete(&self, key: &str) -> Result<bool> {
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
            let seqno = ws.alloc_seqno()?;
            ws.tombstone(rowid, config.clock.now_unix_ms(), seqno)
        })
    }

    /// 按 `RowId` 删除,返回是否命中活记录。
    ///
    /// # Arguments
    /// * `id` - 目标 `RowId`;全库共享同一编号空间,不区分命名空间。
    ///
    /// # Returns
    /// 命中活记录并写入墓碑返回 `true`;`RowId` 不存在或已是墓碑返回 `false`。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{InsertOutcome, Mneme, Record};
    ///
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// let id = match ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap() {
    ///     InsertOutcome::Inserted(id) => id,
    ///     other => panic!("unexpected: {other:?}"),
    /// };
    /// assert!(ns.delete_by_rowid(id).unwrap());
    /// // 墓碑已存在,重复删除返回 `false`。
    /// assert!(!ns.delete_by_rowid(id).unwrap());
    /// ```
    pub fn delete_by_rowid(&self, id: RowId) -> Result<bool> {
        let config = Arc::clone(&self.config);
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            let seqno = ws.alloc_seqno()?;
            ws.tombstone(id, config.clock.now_unix_ms(), seqno)
        })
    }
}
