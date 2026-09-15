//! `Namespace` 信念修订(`namespace/write/supersede.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::types::{Key, SlotId};
use crate::memory::Namespace;
use crate::memory::config::Config;
use crate::memory::record::{Record, UpdateOutcome};
use crate::memory::table::WriterState;
use crate::memory::write_helpers::{SlotSpec, build_slot, validate_insert};

impl Namespace {
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
            supersede_rowid(
                ws,
                SupersedeInput {
                    config: &config,
                    ns_path: &ns_path,
                    key,
                    rec,
                },
            )
        })
    }
}

/// [`supersede_rowid`] 的输入参数。
struct SupersedeInput<'a> {
    /// 库配置(时钟与写入限额)。
    config: &'a Config,
    /// 目标命名空间路径。
    ns_path: &'a Arc<str>,
    /// 要修订的记录键。
    key: &'a str,
    /// 新版本记录。
    rec: Record,
}

/// 信念修订的写事务主体(供 [`Namespace::supersede`] 的 `write_tx` 闭包调用;
/// 抽函数以控制 `supersede` 行数)。
fn supersede_rowid(ws: &mut WriterState, input: SupersedeInput<'_>) -> Result<UpdateOutcome> {
    let SupersedeInput {
        config,
        ns_path,
        key,
        rec,
    } = input;
    if ws.closed {
        return Err(MnemeError::Closed);
    }
    let Some(ns_id) = ws.ns_id_of(ns_path) else {
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
    validate_insert(config, &rec)?;
    let now = config.clock.now_unix_ms();
    let new_valid_from = rec.valid_from.unwrap_or(now);
    let seqno = ws.alloc_seqno()?;
    let slot_data = build_slot(SlotSpec {
        ns_id,
        ns_path: Arc::clone(ns_path),
        rowid,
        seqno,
        tx_ms: now,
        rec,
    });
    // 先提交新版本(容量不足时在遮蔽旧版本前失败),成功后再闭合旧版本 valid_to,
    // 保证提交失败时旧版本保持原样(FC-MEM-PRE-002 零部分写入)。
    ws.commit_version(rowid, slot_data)?;
    close_old_version(ws, latest, new_valid_from)?;
    Ok(UpdateOutcome::Updated(rowid))
}

/// 把旧版本 `valid_to` 闭合到新版本的 `valid_from`(提交新版本成功后调用)。
fn close_old_version(ws: &mut WriterState, latest: SlotId, valid_to: i64) -> Result<()> {
    // reason: `latest` 由 `key_index` 维护、槽位永不复用(FC-MEM-INV-004);越界属内部损坏。
    let old = Arc::make_mut(
        Arc::make_mut(&mut ws.slots)
            .get_mut(latest.get() as usize)
            .ok_or(MnemeError::Inconsistent {
                reason: "latest 槽位越界(FC-MEM-INV-004)",
            })?,
    );
    old.valid_to = Some(valid_to);
    Ok(())
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
