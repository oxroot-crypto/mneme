//! 合并控制:显式 compaction、计划选取与段组替换收尾。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::types::SegmentId;
use crate::life::compact::{self, SegmentInfo};
use crate::memory::engine::Mneme;
use crate::memory::ops::CompactionControl;
use crate::memory::table::WriterState;

impl Mneme {
    /// 以"全部活跃段"为计划强制合并重写一次(密钥轮换/迁移专用)。
    ///
    /// 复用 compaction 的段组替换流程:新段以 active 密钥落盘、MANIFEST 同步重写,
    /// 旧段进入 `trash/`;保留口径与常规 compaction 一致(`history_horizon`)。
    pub(super) fn rewrite_all_segments(&self) -> Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        drop(view);
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        let manifest = store.manifest_snapshot();
        let segments: Vec<u32> = manifest
            .segments
            .iter()
            .map(|segment| segment.segment_id)
            .filter(|id| !ws.unavailable_segments.contains(id))
            .collect();
        if segments.is_empty() {
            return Ok(());
        }
        self.control
            .mark_running(segments.iter().map(|id| SegmentId::new(*id)).collect());
        let plan = crate::memory::ops::CompactionPlan { segments };
        let now_ms = self.config.clock.now_unix_ms();
        let survivors =
            compact::select_survivors(&ws, &plan, now_ms, self.config.compaction.history_horizon);
        let input = crate::persist::store::CompactInput {
            plan: &plan,
            keep_slots: &survivors.keep,
            control: &self.control,
        };
        match store.compact(&mut ws, &self.config, &input) {
            Ok(false) => {
                self.control.mark_idle();
                Ok(())
            }
            Ok(true) => {
                self.finish_compaction(&mut ws, &survivors);
                Ok(())
            }
            Err(error) => {
                self.control.mark_idle();
                Err(error)
            }
        }
    }

    /// 返回后台合并控制句柄(与库共享同一状态)。
    ///
    /// # Returns
    /// 与库共享同一合并状态的 [`CompactionControl`]。
    pub fn compact_control(&self) -> CompactionControl {
        self.control.clone()
    }

    /// 显式执行一轮 size-tiered compaction(设计 07 §4)。
    ///
    /// 选段与幸存版本筛选由 L5 完成;合并段写盘与 MANIFEST 替换由持久层完成。
    /// 无触发条件、已暂停或纯内存库时为空操作。合并期间 `stats().compaction`
    /// 反映 `Running`(运行中暂停则为 `Paused`);`pause()` 在提交前生效,中止时
    /// 不改动任何已提交状态。
    ///
    /// # Returns
    /// 本轮 compaction 完成(含空操作)返回 `Ok(())`。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`];只读模式返回
    /// [`MnemeError::Unsupported`];I/O/编码失败返回结构化错误并回到 `Idle`。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.namespace("demo")
    ///     .insert(Record::new(vec![1.0, 0.0]).key("a"))
    ///     .unwrap();
    /// // 纯内存库无段可合并:空操作。
    /// db.compact().unwrap();
    /// ```
    pub fn compact(&self) -> Result<()> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        drop(view);
        let Some(store) = &self.store else {
            return Ok(());
        };
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        if self.control.is_paused() {
            return Ok(());
        }
        self.run_compaction(&mut ws, store)
    }

    /// 选计划并执行一轮 compaction(调用方持写锁)。
    ///
    /// 单轮在 `CompactionPolicy.io_budget` 预算内**连续**合并段组:输入字节 =
    /// 活跃段文件总字节 × `io_budget`,每组合并后累计其输入字节,超预算即结束
    /// 本轮(`spent == 0` 时无条件执行第一组,保证有进展),余下留待下一次
    /// (FC-LIFE-POST-011)。轮数上限取 2×起始活跃段数 + 1,防统计口径异常时
    /// 无限循环。
    fn run_compaction(
        &self,
        ws: &mut WriterState,
        store: &Arc<crate::persist::store::Store>,
    ) -> Result<()> {
        let total_bytes = store.active_segment_bytes(&ws.unavailable_segments);
        let budget = (total_bytes as f64 * f64::from(self.config.compaction.io_budget)) as u64;
        let mut spent = 0_u64;
        let mut rounds = store.total_segments().saturating_mul(2).saturating_add(1);
        while rounds > 0 {
            if self.control.is_paused() {
                return Ok(());
            }
            rounds -= 1;
            let now_ms = self.config.clock.now_unix_ms();
            let Some(plan) = self.plan_compaction(ws, store, now_ms) else {
                return Ok(());
            };
            let input_bytes = store.segments_bytes(&plan.segments);
            if spent > 0 && spent.saturating_add(input_bytes) > budget {
                return Ok(());
            }
            if self.compact_group(ws, store, &plan, now_ms)? {
                spent = spent.saturating_add(input_bytes);
            } else {
                return Ok(());
            }
        }
        Ok(())
    }

    /// 标记运行态并合并一个段组:`Ok(true)` 表示已提交,`Ok(false)` 表示暂停中止
    /// (控制状态已复位,段集与内存状态都不动)。
    fn compact_group(
        &self,
        ws: &mut WriterState,
        store: &Arc<crate::persist::store::Store>,
        plan: &crate::memory::ops::CompactionPlan,
        now_ms: i64,
    ) -> Result<bool> {
        self.control
            .mark_running(plan.segments.iter().map(|id| SegmentId::new(*id)).collect());
        if self.control.is_paused() {
            self.control.mark_idle();
            return Ok(false);
        }
        let survivors =
            compact::select_survivors(ws, plan, now_ms, self.config.compaction.history_horizon);
        let input = crate::persist::store::CompactInput {
            plan,
            keep_slots: &survivors.keep,
            control: &self.control,
        };
        match store.compact(ws, &self.config, &input) {
            Ok(false) => {
                // 暂停中止:段集与内存状态都不动。
                self.control.mark_idle();
                Ok(false)
            }
            Ok(true) => {
                self.finish_compaction(ws, &survivors);
                Ok(true)
            }
            Err(error) => {
                self.control.mark_idle();
                Err(error)
            }
        }
    }

    /// 由当前 MANIFEST 与写状态选出本轮合并计划;无触发条件时 `None`。
    fn plan_compaction(
        &self,
        ws: &WriterState,
        store: &Arc<crate::persist::store::Store>,
        now_ms: i64,
    ) -> Option<crate::memory::ops::CompactionPlan> {
        let manifest = store.manifest_snapshot();
        let infos: Vec<SegmentInfo> = manifest
            .segments
            .iter()
            // 损坏隔离段(内存跳过、文件原地保留)绝不参与合并,否则会被当活跃段
            // 清除,数据同修复机会一齐冇(FC-PERSIST-ERR-006)。
            .filter(|segment| !ws.unavailable_segments.contains(&segment.segment_id))
            .map(|segment| SegmentInfo {
                id: segment.segment_id,
                rows: segment.row_count,
            })
            .collect();
        let dead = compact::segment_dead_ratios(ws, now_ms, self.config.compaction.history_horizon);
        compact::plan(&infos, &dead, &self.config.compaction)
    }

    /// 提交成功后的收尾:剪除已回收版本、同步内存统计并发布视图。
    fn finish_compaction(&self, ws: &mut WriterState, survivors: &compact::SurvivorSet) {
        ws.prune_reclaimed(&survivors.reclaim);
        ws.note_reclaimed(survivors.reclaim.len());
        for &index in &survivors.keep {
            let rowid = ws.slots[index].rowid;
            // 只有该 RowId 的**最新版本**本次被物化(版本行的 access 是累计快照)
            // 才能清 dirty;仅历史版本入段时,增量必须留给下一段 delta,否则访问
            // 计数丢失(FC-PERSIST-POST-010)。
            let latest_materialized = ws.latest.get(&rowid).is_some_and(|latest| {
                survivors
                    .keep
                    .binary_search(&(latest.get() as usize))
                    .is_ok()
            });
            if latest_materialized {
                Arc::make_mut(&mut ws.access_dirty).remove(&rowid);
            }
        }
        self.table.publish(ws);
        self.control.mark_idle();
    }
}
