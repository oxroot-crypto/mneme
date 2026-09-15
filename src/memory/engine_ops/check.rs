//! fsck:索引一致性与持久段完整性校验,附运维建议。

use crate::core::error::{MnemeError, Result};
use crate::memory::engine::Mneme;
use crate::memory::ops::CheckReport;
use crate::memory::table::ReaderView;

use super::stats::dead_ratios;

impl Mneme {
    /// fsck:校验内部索引一致性与持久段完整性,并给出运维建议。
    ///
    /// 内存侧:仅当 key 索引指向不存在的物理版本,或最新版本 `ns_id`/`key` 与索引
    /// 不符时报告不一致;已删除(墓碑)与已逻辑过期的记录不算不一致(FC-MEM-POST-008)。
    /// 持久侧(L2):逐段校验头部/payload CRC 与版本链记录体(设计 16 §1.6)。
    /// 另报告每段墓碑/过期占比与合并建议(FC-LIFE-POST-009;建议不影响 `ok`)。
    ///
    /// # Returns
    /// fsck 报告:`ok` 为 `true` 表示未发现索引不一致或段损坏;`corrupted` 与
    /// `suggestions` 给出损坏段与运维建议。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.namespace("demo")
    ///     .insert(Record::new(vec![1.0, 0.0]).key("a"))
    ///     .unwrap();
    /// assert!(db.check().unwrap().ok);
    /// db.namespace("demo").delete("a").unwrap();
    /// assert!(db.check().unwrap().ok);
    /// ```
    pub fn check(&self) -> Result<CheckReport> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let now = self.config.clock.now_unix_ms();
        let mut suggestions = Vec::new();
        let mut corrupted = Vec::new();
        // L2:逐段校验头部/payload CRC 与版本链(设计 16 §1.6)。
        if let Some(store) = &self.store {
            for id in store.verify_segments() {
                suggestions.push(format!("段 {id} 校验失败"));
                corrupted.push(id);
            }
        }
        let inconsistent = check_key_index(&view, &mut suggestions);
        append_fsck_suggestions(self, &view, now, &mut suggestions);
        Ok(CheckReport {
            ok: corrupted.is_empty() && !inconsistent,
            corrupted,
            suggestions,
        })
    }
}

/// 按最新物理版本对账 key 索引;发现不一致时追加建议并返回 `true`。
///
/// 墓碑/逻辑过期不算不一致(FC-MEM-POST-008)。
fn check_key_index(view: &ReaderView, suggestions: &mut Vec<String>) -> bool {
    let mut inconsistent = false;
    for ((ns_id, key), rowid) in view.key_index.iter() {
        match view.latest.get(rowid) {
            Some(slot) => {
                let slot_data = &view.slots[slot.get() as usize];
                if slot_data.ns_id != *ns_id || slot_data.key.as_ref() != Some(key) {
                    inconsistent = true;
                    suggestions.push(format!("key 索引不一致:{key}"));
                }
            }
            None => {
                inconsistent = true;
                suggestions.push(format!("key 索引指向不存在的版本:{key}"));
            }
        }
    }
    inconsistent
}

/// 运维建议:死比率超线的段与可合并段数(仅提示,不影响 `ok`)。
///
/// 死比率建议仅在 `history_horizon` 有限时给出——默认 `None` 时没有任何版本
/// 可回收,给"建议合并回收"会与实际触发口径矛盾(FC-LIFE-POST-009)。
fn append_fsck_suggestions(
    engine: &Mneme,
    view: &ReaderView,
    now: i64,
    suggestions: &mut Vec<String>,
) {
    if engine.config.compaction.history_horizon.is_some() {
        let ratios = dead_ratios(view, now);
        for (id, ratio) in &ratios {
            if *ratio > engine.config.compaction.dead_ratio {
                suggestions.push(format!(
                    "段 {id} 墓碑/过期占比 {:.0}%,建议合并回收",
                    ratio * 100.0
                ));
            }
        }
    }
    if let Some(store) = &engine.store {
        let total = store.total_segments();
        if total >= engine.config.compaction.tier_count.max(2) as usize {
            suggestions.push(format!("建议合并 {total} 个段"));
        }
    }
}
