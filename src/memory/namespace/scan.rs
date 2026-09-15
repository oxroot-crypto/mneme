//! `Namespace` 过滤遍历与计数(`namespace/scan.rs`)。
//!
//! 遍历非流式:调用前物化命中行的 `Arc` 句柄、不复制记录体(FC-MEM-CPLX-005);
//! 过滤走三值语义,墓碑/逻辑过期记录默认不可见(I9)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::types::{NsId, RowId};
use crate::memory::pred::{self, EvalCtx, Expr};
use crate::memory::record::RecordRef;
use crate::memory::table::{ReaderView, SlotData};

use super::Namespace;

impl Namespace {
    /// 统计命中活记录数(不物化记录)。
    ///
    /// # Arguments
    /// * `filter` - 三值过滤表达式;`None` 表示不过滤。
    ///
    /// # Returns
    /// 命中过滤条件的活记录数;命名空间未注册返回 `0`。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0])).unwrap();
    /// ns.insert(Record::new(vec![0.0, 1.0])).unwrap();
    /// assert_eq!(ns.count(None).unwrap(), 2);
    /// ```
    pub fn count(&self, filter: Option<Expr>) -> Result<u64> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let now = self.config.clock.now_unix_ms();
        let Some(ns_id) = ns_id_of(&view, &self.ns_path) else {
            return Ok(0);
        };
        let mut count = 0;
        let uses_access = filter.as_ref().is_some_and(pred::Expr::uses_access);
        for (idx, slot) in view.slots.iter().enumerate() {
            if view.dead.get(idx) || slot.ns_id != ns_id || !slot.is_live(now) {
                continue;
            }
            if !passes_filter(PassesFilterInput {
                filter: &filter,
                slot_data: slot.as_ref(),
                view: &view,
                rowid: &slot.rowid,
                uses_access,
            }) {
                continue;
            }
            count += 1;
        }
        Ok(count)
    }

    /// 按过滤条件遍历(不含墓碑/过期记录),调用前物化命中行的 `Arc` 句柄、
    /// 不复制记录体(FC-MEM-CPLX-005,非流式)。
    ///
    /// # Arguments
    /// * `filter` - 三值过滤表达式;`None` 表示不过滤。
    ///
    /// # Returns
    /// 按 `RowId` 升序产出 `Ok(RecordRef)` 的迭代器(内层 `Err` 为 L2 预留)。
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
    /// let keys: Vec<String> = ns
    ///     .iter(None)
    ///     .unwrap()
    ///     .map(|record| record.unwrap().key().unwrap().to_string())
    ///     .collect();
    /// assert_eq!(keys, vec!["a"]);
    /// ```
    pub fn iter(
        &self,
        filter: Option<Expr>,
    ) -> Result<impl Iterator<Item = Result<RecordRef<'_>>> + '_> {
        self.iter_with(filter, false)
    }

    /// 按过滤条件遍历(导出/审计/重建用);`include_deleted=true` 时包含墓碑/过期记录(仅审计)。
    ///
    /// # Arguments
    /// * `filter` - 三值过滤表达式;`None` 表示不过滤。
    /// * `include_deleted` - `true` 时包含墓碑/已逻辑过期记录。
    ///
    /// # Returns
    /// 按 `RowId` 升序产出 `Ok(RecordRef)` 的迭代器(内层 `Err` 为 L2 预留)。
    /// 调用前物化命中行的 `Arc` 句柄、不复制记录体(FC-MEM-CPLX-005,非流式)。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    pub fn iter_with(
        &self,
        filter: Option<Expr>,
        include_deleted: bool,
    ) -> Result<impl Iterator<Item = Result<RecordRef<'_>>> + '_> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let now = self.config.clock.now_unix_ms();
        let mut collected = Vec::new();
        let uses_access = filter.as_ref().is_some_and(pred::Expr::uses_access);
        if let Some(ns_id) = ns_id_of(&view, &self.ns_path) {
            for (rowid, slot) in view.latest.iter() {
                let slot_data = &view.slots[slot.get() as usize];
                if slot_data.ns_id != ns_id {
                    continue;
                }
                if !include_deleted
                    && (view.dead.get(slot.get() as usize) || !slot_data.is_live(now))
                {
                    continue;
                }
                if !passes_filter(PassesFilterInput {
                    filter: &filter,
                    slot_data: slot_data.as_ref(),
                    view: &view,
                    rowid,
                    uses_access,
                }) {
                    continue;
                }
                collected.push(Arc::clone(slot_data));
            }
        }
        collected.sort_by_key(|slot_data| slot_data.rowid);
        Ok(collected
            .into_iter()
            .map(|slot_data| Ok(RecordRef::new(slot_data))))
    }
}

/// 解析命名空间路径对应的 `NsId`。
fn ns_id_of(view: &ReaderView, ns_path: &str) -> Option<NsId> {
    view.ns_by_path.get(ns_path).copied()
}

/// [`passes_filter`] 的输入参数。
struct PassesFilterInput<'a> {
    /// 三值过滤表达式(`None` = 不过滤)。
    filter: &'a Option<Expr>,
    /// 待判定的物理槽位。
    slot_data: &'a SlotData,
    /// 不可变读视图(访问统计查表用)。
    view: &'a ReaderView,
    /// 槽位行标识(访问统计主键)。
    rowid: &'a RowId,
    /// 表达式是否引用访问统计(调用方在循环外预判定)。
    uses_access: bool,
}

/// 判断记录是否通过可选过滤表达式(三值语义,缺失字段不命中)。
///
/// `uses_access` 由调用方在循环外预判定:表达式未引用访问统计时不查访问表。
fn passes_filter(input: PassesFilterInput<'_>) -> bool {
    let PassesFilterInput {
        filter,
        slot_data,
        view,
        rowid,
        uses_access,
    } = input;
    match filter {
        Some(expr) => pred::matches(
            expr,
            &EvalCtx {
                slot: slot_data,
                access: uses_access
                    .then(|| view.access.get(rowid).copied())
                    .flatten(),
            },
        ),
        None => true,
    }
}
