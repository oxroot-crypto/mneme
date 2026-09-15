//! 快照视图的计数与三值过滤求值。

use crate::core::error::Result;
use crate::core::types::RowId;
use crate::memory::pred::{self, EvalCtx, Expr};
use crate::memory::table::{ReaderView, SlotData};

use super::namespace::SnapshotNamespace;

impl SnapshotNamespace {
    /// 统计命中活记录数。
    ///
    /// # Arguments
    /// * `filter` - 三值过滤表达式;`None` 表示不过滤。
    ///
    /// # Returns
    /// 命中过滤条件的活记录数;命名空间未注册返回 `0`。逻辑过期以**快照时刻
    /// `as_of_ms`** 判定(FC-QUERY-POST-006)。
    ///
    /// # Errors
    /// 当前恒 `Ok`(视图被钉住、不探测关闭态;`Result` 为 L2 持久层错误预留)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.namespace("demo")
    ///     .insert(Record::new(vec![1.0, 0.0]))
    ///     .unwrap();
    /// let snap = db.snapshot().namespace("demo");
    /// assert_eq!(snap.count(None).unwrap(), 1);
    /// ```
    pub fn count(&self, filter: Option<Expr>) -> Result<u64> {
        let Some(ns_id) = self.ns_id() else {
            return Ok(0);
        };
        let now = self.as_of_ms;
        let mut count = 0;
        for (idx, slot) in self.view.slots.iter().enumerate() {
            if self.view.dead.get(idx) || slot.ns_id != ns_id || !slot.is_live(now) {
                continue;
            }
            if !passes_filter(&filter, slot, &self.view, &slot.rowid) {
                continue;
            }
            count += 1;
        }
        Ok(count)
    }
}

/// 判断记录是否通过可选过滤表达式(三值语义,缺失字段不命中)。
pub(crate) fn passes_filter(
    filter: &Option<Expr>,
    slot_data: &SlotData,
    view: &ReaderView,
    rowid: &RowId,
) -> bool {
    match filter {
        Some(expr) => pred::matches(
            expr,
            &EvalCtx {
                slot: slot_data,
                access: view.access.get(rowid).copied(),
            },
        ),
        None => true,
    }
}
