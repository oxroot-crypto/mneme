//! `Namespace` 写路径(`namespace/write.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::options::UpdatePatch;
use crate::core::types::{Key, RowId};
use crate::memory::mutate_helpers::update_rowid;
use crate::memory::record::{InsertOutcome, Record, UpdateOutcome};
use crate::memory::write_helpers::{InsertCtx, SlotSpec, build_slot, insert_one, validate_insert};

use super::Namespace;
impl Namespace {
    /// 单条写入。
    ///
    /// # Arguments
    /// * `rec` - 待写入记录;向量维度须与建库维度一致,分量必须是有限值。
    ///
    /// # Returns
    /// 逐条写入结果:新建 [`InsertOutcome::Inserted`]、去重合并
    /// [`InsertOutcome::Merged`](保留旧 `RowId`)、`RejectDuplicate` 命中
    /// [`InsertOutcome::Duplicate`]。
    ///
    /// # Errors
    /// 维度不符 → [`MnemeError::DimensionMismatch`];向量分量或 `importance`/`confidence`
    /// 非有限值 → [`MnemeError::NonFinite`];超限 → [`MnemeError::TooLarge`]/
    /// [`MnemeError::MetaTooDeep`];`RejectDuplicate` 命中 →
    /// [`MnemeError::DuplicateKey`];库已关闭 → [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// assert!(ns.exists("a").unwrap());
    /// ```
    pub fn insert(&self, rec: Record) -> Result<InsertOutcome> {
        let config = Arc::clone(&self.config);
        let ns_path = Arc::clone(&self.ns_path);
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            let ns_id = ws.register_ns(&ns_path)?;
            insert_one(
                ws,
                &config,
                InsertCtx {
                    ns_id,
                    ns_path,
                    rec,
                    now: config.clock.now_unix_ms(),
                    in_batch: false,
                },
            )
        })
    }

    /// 批量写入,整批原子(不变量 I15)。
    ///
    /// 任一条维度/数值/限额校验失败则整批拒绝;预校验后逐条求值阶段仍失败时
    /// (如 `Dedup::Merge` 回调产物超限)以写状态快照回滚,保证零部分写入;
    /// 去重命中与 `RejectDuplicate` 为逐条结果,不回滚整批。
    ///
    /// # Arguments
    /// * `recs` - 批量记录;按顺序逐条求值,返回顺序与输入一致。
    ///
    /// # Returns
    /// 与 `recs` 等长、顺序一致的逐条结果;批内重复 key(`RejectDuplicate`)以
    /// [`InsertOutcome::Duplicate`] 出现在对应位置,不算错误。
    ///
    /// # Errors
    /// 任一条维度不符 → [`MnemeError::DimensionMismatch`];分量非有限值 →
    /// [`MnemeError::NonFinite`];超限 → [`MnemeError::TooLarge`]/[`MnemeError::MetaTooDeep`];
    /// 库已关闭 → [`MnemeError::Closed`]。批内 key 重复(`RejectDuplicate`)以
    /// [`InsertOutcome::Duplicate`] 结果返回,不算错误。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// let outcomes = ns
    ///     .insert_batch(vec![
    ///         Record::new(vec![1.0, 0.0]),
    ///         Record::new(vec![0.0, 1.0]),
    ///     ])
    ///     .unwrap();
    /// assert_eq!(outcomes.len(), 2);
    /// ```
    pub fn insert_batch(&self, recs: Vec<Record>) -> Result<Vec<InsertOutcome>> {
        let config = Arc::clone(&self.config);
        let ns_path = Arc::clone(&self.ns_path);
        // 事务快照:预校验后 `insert_one` 仍可能失败(`Dedup::Merge` 回调产物超限、
        // 槽位容量溢出)。`write_tx` 回滚到批前状态(含命名空间登记),保证整批
        // 零部分写入(FC-MEM-POST-002)。
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            for rec in &recs {
                validate_insert(&config, rec)?;
            }
            let ns_id = ws.register_ns(&ns_path)?;
            let now = config.clock.now_unix_ms();
            let mut outcomes = Vec::with_capacity(recs.len());
            for rec in recs {
                let outcome = insert_one(
                    ws,
                    &config,
                    InsertCtx {
                        ns_id,
                        ns_path: Arc::clone(&ns_path),
                        rec,
                        now,
                        in_batch: true,
                    },
                )?;
                outcomes.push(outcome);
            }
            Ok(outcomes)
        })
    }

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
    pub fn update_by_rowid(&self, id: RowId, patch: UpdatePatch) -> Result<UpdateOutcome> {
        let config = Arc::clone(&self.config);
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            update_rowid(ws, &config, id, &patch)
        })
    }

    /// 信念修订:更新同 key,并把旧版本 `valid_to` 闭合为新版本 `valid_from`。
    ///
    /// # Arguments
    /// * `key` - 要修订的记录键;不存在时返回 `NotFound`。
    /// * `rec` - 新版本记录;其 `valid_from`(缺省为当前时刻)同时作为旧版本的 `valid_to`。
    ///
    /// # Returns
    /// 修订成功返回 [`UpdateOutcome::Updated`](沿用原 `RowId`);key 不存在、
    /// 命名空间未注册或已墓碑返回 [`UpdateOutcome::NotFound`]。
    ///
    /// # Errors
    /// 新记录维度不符 → [`MnemeError::DimensionMismatch`];向量分量或
    /// `importance`/`confidence` 非有限值 → [`MnemeError::NonFinite`];超限 →
    /// [`MnemeError::TooLarge`]/[`MnemeError::MetaTooDeep`];库已关闭 →
    /// [`MnemeError::Closed`]。key 不存在或已墓碑时返回
    /// `Ok(UpdateOutcome::NotFound)`,不算错误;墓碑绝不因 `supersede` 复活
    /// (与 `update` 同口径,FC-MODEL-POST-003)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// ns.supersede("a", Record::new(vec![0.0, 1.0]).key("a"))
    ///     .unwrap();
    /// ```
    pub fn supersede(&self, key: &str, rec: Record) -> Result<UpdateOutcome> {
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
            let Some(latest) = ws.latest.get(&rowid).copied() else {
                return Ok(UpdateOutcome::NotFound);
            };
            // 信念修订是 update 同 key 的语义糖:已墓碑记录与 `update` 同口径返回
            // `NotFound`,绝不复活(FC-MODEL-POST-003);逻辑过期不阻止修订。
            if ws.slots[latest.get() as usize].deleted {
                return Ok(UpdateOutcome::NotFound);
            }
            // 信念修订 = update 同 key:新版本沿用目标 key;显式冲突 → `KeyMismatch`。
            let rec = reconcile_supersede_key(rec, key)?;
            validate_insert(&config, &rec)?;
            let now = config.clock.now_unix_ms();
            let new_valid_from = rec.valid_from.unwrap_or(now);
            let seqno = ws.alloc_seqno()?;
            let slot_data = build_slot(SlotSpec {
                ns_id,
                ns_path,
                rowid,
                seqno,
                tx_ms: now,
                rec,
            });
            // 先提交新版本(容量不足时在遮蔽旧版本前失败),成功后再闭合旧版本 valid_to,
            // 保证提交失败时旧版本保持原样(FC-MEM-PRE-002 零部分写入)。
            ws.commit_version(rowid, slot_data)?;
            {
                let old = Arc::make_mut(
                    Arc::make_mut(&mut ws.slots)
                        .get_mut(latest.get() as usize)
                        .expect("latest 槽位必在界内(FC-MEM-INV-004)"),
                );
                old.valid_to = Some(new_valid_from);
            }
            Ok(UpdateOutcome::Updated(rowid))
        })
    }
}

/// 信念修订须沿用目标 key:新记录省略 key 时继承首参 key;显式给出且冲突 →
/// [`MnemeError::KeyMismatch`],绝不静默改 key(FC-MODEL-POST-003)。
fn reconcile_supersede_key(mut rec: Record, key: &str) -> Result<Record> {
    match rec.key.as_deref() {
        None => rec.key = Some(key.to_string()),
        Some(existing) if existing != key => {
            return Err(MnemeError::KeyMismatch {
                expected: Key::new(key),
                got: Key::new(existing),
            });
        }
        Some(_) => {}
    }
    Ok(rec)
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
