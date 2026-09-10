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
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        let ns_id = ws.register_ns(&self.ns_path);
        let ns_path = Arc::clone(&self.ns_path);
        let now = self.config.clock.now_unix_ms();
        let outcome = insert_one(
            &mut ws,
            &self.config,
            InsertCtx {
                ns_id,
                ns_path,
                rec,
                now,
                in_batch: false,
            },
        )?;
        self.table.publish(&ws);
        Ok(outcome)
    }

    /// 批量写入,整批原子(不变量 I15)。
    ///
    /// 任一条维度/数值/限额校验失败则整批拒绝;去重命中与 `RejectDuplicate`
    /// 为逐条结果,不回滚整批。
    ///
    /// # Arguments
    /// * `recs` - 批量记录;按顺序逐条求值,返回顺序与输入一致。
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
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        for rec in &recs {
            validate_insert(&self.config, rec)?;
        }
        let ns_id = ws.register_ns(&self.ns_path);
        let ns_path = Arc::clone(&self.ns_path);
        let now = self.config.clock.now_unix_ms();
        let mut outcomes = Vec::with_capacity(recs.len());
        for rec in recs {
            outcomes.push(insert_one(
                &mut ws,
                &self.config,
                InsertCtx {
                    ns_id,
                    ns_path: Arc::clone(&ns_path),
                    rec,
                    now,
                    in_batch: true,
                },
            )?);
        }
        self.table.publish(&ws);
        Ok(outcomes)
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
        let seqno = ws.alloc_seqno();
        let removed = ws.tombstone(rowid, now, seqno)?;
        self.table.publish(&ws);
        Ok(removed)
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
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        let now = self.config.clock.now_unix_ms();
        let seqno = ws.alloc_seqno();
        let removed = ws.tombstone(id, now, seqno)?;
        self.table.publish(&ws);
        Ok(removed)
    }

    /// 保留 `RowId` 的局部更新(按 key 定位)。
    ///
    /// # Arguments
    /// * `key` - 记录键;按当前命名空间隔离查找。
    /// * `patch` - 局部补丁;仅 `Some` 字段生效,向量字段受维度与非有限值校验。
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
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        let Some(ns_id) = ws.ns_id_of(&self.ns_path) else {
            return Ok(UpdateOutcome::NotFound);
        };
        let Some(rowid) = ws.key_index.get(&(ns_id, Key::new(key))).copied() else {
            return Ok(UpdateOutcome::NotFound);
        };
        let outcome = update_rowid(&mut ws, &self.config, rowid, &patch)?;
        self.table.publish(&ws);
        Ok(outcome)
    }

    /// 保留 `RowId` 的局部更新(按 `RowId` 定位)。
    ///
    /// # Arguments
    /// * `id` - 目标 `RowId`;不存在或已墓碑时返回 `NotFound`。
    /// * `patch` - 局部补丁;仅 `Some` 字段生效。
    ///
    /// # Errors
    /// 库已关闭 → [`MnemeError::Closed`];补丁向量维度不符 →
    /// [`MnemeError::DimensionMismatch`];向量分量或 `importance`/`confidence` 非有限值 →
    /// [`MnemeError::NonFinite`];text/metadata/provenance 超限 →
    /// [`MnemeError::TooLarge`]/[`MnemeError::MetaTooDeep`]。
    /// `RowId` 不存在时返回 `Ok(UpdateOutcome::NotFound)`,不算错误。
    pub fn update_by_rowid(&self, id: RowId, patch: UpdatePatch) -> Result<UpdateOutcome> {
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        let outcome = update_rowid(&mut ws, &self.config, id, &patch)?;
        self.table.publish(&ws);
        Ok(outcome)
    }

    /// 信念修订:更新同 key,并把旧版本 `valid_to` 闭合为新版本 `valid_from`。
    ///
    /// # Arguments
    /// * `key` - 要修订的记录键;不存在时返回 `NotFound`。
    /// * `rec` - 新版本记录;其 `valid_from`(缺省为当前时刻)同时作为旧版本的 `valid_to`。
    ///
    /// # Errors
    /// 新记录维度不符 → [`MnemeError::DimensionMismatch`];向量分量或
    /// `importance`/`confidence` 非有限值 → [`MnemeError::NonFinite`];超限 →
    /// [`MnemeError::TooLarge`]/[`MnemeError::MetaTooDeep`];库已关闭 →
    /// [`MnemeError::Closed`]。key 不存在时返回
    /// `Ok(UpdateOutcome::NotFound)`,不算错误。
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
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        let Some(ns_id) = ws.ns_id_of(&self.ns_path) else {
            return Ok(UpdateOutcome::NotFound);
        };
        let Some(rowid) = ws.key_index.get(&(ns_id, Key::new(key))).copied() else {
            return Ok(UpdateOutcome::NotFound);
        };
        let Some(latest) = ws.latest.get(&rowid).copied() else {
            return Ok(UpdateOutcome::NotFound);
        };
        validate_insert(&self.config, &rec)?;
        let now = self.config.clock.now_unix_ms();
        let new_valid_from = rec.valid_from.unwrap_or(now);
        // 闭合旧版本 valid_to(写者独占,可安全就地改写)。
        {
            let slots = Arc::make_mut(&mut ws.slots);
            let old = Arc::make_mut(&mut slots[latest.get() as usize]);
            old.valid_to = Some(new_valid_from);
        }
        let seqno = ws.alloc_seqno();
        let slot_data = build_slot(SlotSpec {
            ns_id,
            ns_path: Arc::clone(&self.ns_path),
            rowid,
            seqno,
            tx_ms: now,
            rec,
        });
        ws.commit_version(rowid, slot_data)?;
        self.table.publish(&ws);
        Ok(UpdateOutcome::Updated(rowid))
    }
}
