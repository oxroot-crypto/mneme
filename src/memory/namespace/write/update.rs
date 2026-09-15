//! `Namespace` 保留 `RowId` 的局部更新(`namespace/write/update.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::options::UpdatePatch;
use crate::core::types::{Key, RowId};
use crate::memory::Namespace;
use crate::memory::mutate_helpers::update_rowid;
use crate::memory::record::UpdateOutcome;

#[cfg(test)]
use crate::memory::record::{InsertOutcome, Record};

impl Namespace {
    /// 保留 `RowId` 的局部更新(按 key 定位)。
    ///
    /// # Arguments
    /// * `key` - 记录键;按当前命名空间隔离查找。
    /// * `patch` - 局部补丁;仅 `Some` 字段生效,向量字段受维度与非有限值校验。
    ///
    /// # Returns
    /// 更新成功返回 [`UpdateOutcome::Updated`](携带命中 `RowId`);key 不存在或
    /// 命名空间未注册返回 [`UpdateOutcome::NotFound`]。
    ///
    /// # Errors
    /// 库已关闭 → [`MnemeError::Closed`];补丁向量维度不符 →
    /// [`MnemeError::DimensionMismatch`];向量分量或 `importance`/`confidence` 非有限值 →
    /// [`MnemeError::NonFinite`];text/metadata/provenance 超限 →
    /// [`MnemeError::TooLarge`]/[`MnemeError::MetaTooDeep`](FC-MEM-PRE-002:与 insert
    /// 同口径,校验失败时记录保持上一版本原样)。
    /// key 不存在时返回 `Ok(UpdateOutcome::NotFound)`,不算错误。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record, UpdatePatch};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// ns.update(
    ///     "a",
    ///     UpdatePatch {
    ///         importance: Some(0.9),
    ///         ..UpdatePatch::default()
    ///     },
    /// )
    /// .unwrap();
    /// ```
    pub fn update(&self, key: &str, patch: UpdatePatch) -> Result<UpdateOutcome> {
        let config = Arc::clone(&self.config);
        let ns_path = Arc::clone(&self.ns_path);
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            let Some(ns_id) = ws.ns_id_of(&ns_path) else {
                return Ok(UpdateOutcome::NotFound);
            };
            let Some(rowid) = ws.key_index.get(&(ns_id, Key::new(key))).copied() else {
                return Ok(UpdateOutcome::NotFound);
            };
            update_rowid(ws, &config, rowid, &patch)
        })
    }

    /// 保留 `RowId` 的局部更新(按 `RowId` 定位)。
    ///
    /// # Arguments
    /// * `id` - 目标 `RowId`;不存在或已墓碑时返回 `NotFound`。
    /// * `patch` - 局部补丁;仅 `Some` 字段生效。
    ///
    /// # Returns
    /// 更新成功返回 [`UpdateOutcome::Updated`];`RowId` 不存在或已墓碑返回
    /// [`UpdateOutcome::NotFound`]。
    ///
    /// # Errors
    /// 库已关闭 → [`MnemeError::Closed`];补丁向量维度不符 →
    /// [`MnemeError::DimensionMismatch`];向量分量或 `importance`/`confidence` 非有限值 →
    /// [`MnemeError::NonFinite`];text/metadata/provenance 超限 →
    /// [`MnemeError::TooLarge`]/[`MnemeError::MetaTooDeep`]。
    /// `RowId` 不存在时返回 `Ok(UpdateOutcome::NotFound)`,不算错误。
    ///
    /// # Examples
    /// ```
    /// use mneme::{InsertOutcome, Mneme, Record, UpdateOutcome, UpdatePatch};
    ///
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// let id = match ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap() {
    ///     InsertOutcome::Inserted(id) => id,
    ///     other => panic!("unexpected: {other:?}"),
    /// };
    /// let patch = UpdatePatch {
    ///     importance: Some(0.9),
    ///     ..UpdatePatch::default()
    /// };
    /// assert_eq!(ns.update_by_rowid(id, patch).unwrap(), UpdateOutcome::Updated(id));
    /// assert_eq!(ns.get_by_rowid(id).unwrap().unwrap().importance(), 0.9);
    /// ```
    pub fn update_by_rowid(&self, id: RowId, patch: UpdatePatch) -> Result<UpdateOutcome> {
        let config = Arc::clone(&self.config);
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            update_rowid(ws, &config, id, &patch)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::engine::Mneme;

    fn inserted(outcome: InsertOutcome) -> RowId {
        match outcome {
            InsertOutcome::Inserted(id) | InsertOutcome::Merged(id) => id,
            other => panic!("期望写入,得到 {other:?}"),
        }
    }

    #[test]
    fn update_by_rowid_updates_visible_and_reports_missing() {
        let db = Mneme::in_memory(2).expect("in_memory");
        let ns = db.namespace("t");
        let id = inserted(
            ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
                .expect("insert"),
        );
        let patch = UpdatePatch {
            importance: Some(0.75),
            ..UpdatePatch::default()
        };
        assert_eq!(
            ns.update_by_rowid(id, patch).expect("update"),
            UpdateOutcome::Updated(id)
        );
        let record = ns.get_by_rowid(id).expect("get").expect("可见");
        assert_eq!(record.importance(), 0.75);

        let tombstoned = inserted(
            ns.insert(Record::new(vec![0.0, 1.0]).key("b"))
                .expect("insert"),
        );
        assert!(ns.delete_by_rowid(tombstoned).expect("delete"));
        assert_eq!(
            ns.update_by_rowid(tombstoned, UpdatePatch::default())
                .expect("update"),
            UpdateOutcome::NotFound
        );
    }
}
