//! `Namespace` 单条与批量写入(`namespace/write/insert.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::memory::Namespace;
use crate::memory::record::{InsertOutcome, Record};
use crate::memory::write_helpers::{InsertCtx, insert_one, validate_insert};

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
}
