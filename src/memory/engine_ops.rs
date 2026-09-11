//! 内存引擎的统计与自检门面(`engine_ops.rs`)。
//!
//! 以 `impl Mneme` 扩展 [`Mneme`](crate::memory::engine::Mneme) 的运维方法
//! (运行统计、fsck、合并控制、落盘空操作),与库生命周期方法分置于不同文件。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::types::SegmentId;
use crate::life::compact::{self, SegmentInfo};
use crate::memory::engine::Mneme;
use crate::memory::ops::{
    CheckReport, CompactionControl, HistoryStat, NsStat, QuantStat, Stats, StorageStat,
};
use crate::memory::table::{ReaderView, WriterState};

/// 持久后端的段/WAL/trash 统计。
#[derive(Default)]
struct StoreStats {
    segments: Vec<crate::memory::ops::SegmentStat>,
    wal_bytes: u64,
    trash_bytes: u64,
    total_segments: usize,
}

/// 聚合各命名空间活行数与文本总长(逻辑过期与墓碑不计入,FC-LIFE-INV-009)。
fn aggregate_namespaces(view: &ReaderView, now: i64) -> (HashMap<String, NsStat>, u64) {
    let mut per_namespace: HashMap<String, NsStat> = HashMap::new();
    let mut live_rows = 0_u64;
    for (idx, slot) in view.slots.iter().enumerate() {
        if view.dead.get(idx) || !slot.is_live(now) {
            continue;
        }
        live_rows += 1;
        let stat = per_namespace.entry(slot.ns_path.to_string()).or_default();
        stat.doc_count += 1;
        stat.total_doc_len += slot.text.as_ref().map_or(0, |text| text.len() as u64);
    }
    (per_namespace, live_rows)
}

/// 统计每段"墓碑 + 逻辑过期"占比(按视图的槽位归属;无段归属的尾部槽位不计)。
fn dead_ratios(view: &ReaderView, now: i64) -> HashMap<u32, f32> {
    let mut dead: HashMap<u32, u64> = HashMap::new();
    let mut total: HashMap<u32, u64> = HashMap::new();
    for (index, segment) in view.slot_segment.iter().enumerate() {
        let Some(id) = segment else {
            continue;
        };
        *total.entry(*id).or_insert(0) += 1;
        let slot = &view.slots[index];
        if view.dead.get(index) || slot.deleted || !slot.is_live(now) {
            *dead.entry(*id).or_insert(0) += 1;
        }
    }
    total
        .into_iter()
        .map(|(id, count)| {
            let dead_count = dead.get(&id).copied().unwrap_or(0);
            let ratio = if count == 0 {
                0.0
            } else {
                dead_count as f32 / count as f32
            };
            (id, ratio)
        })
        .collect()
}

impl Mneme {
    /// 返回运行统计。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.namespace("demo")
    ///     .insert(Record::new(vec![1.0, 0.0]))
    ///     .unwrap();
    /// let stats = db.stats().unwrap();
    /// assert_eq!(stats.per_namespace["demo"].doc_count, 1);
    /// ```
    pub fn stats(&self) -> Result<Stats> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let now = self.config.clock.now_unix_ms();
        let (per_namespace, live_rows) = aggregate_namespaces(&view, now);
        let relations = view.out_edges.values().map(Vec::len).sum::<usize>() as u64;
        let mut store = self.collect_store_stats(&view);
        let dead = dead_ratios(&view, now);
        for segment in &mut store.segments {
            if let Some(ratio) = dead.get(&segment.id.get()) {
                segment.dead_ratio = *ratio;
            }
        }
        Ok(Stats {
            segments: store.segments,
            wal_bytes: store.wal_bytes,
            memory_est: live_rows
                * u64::from(self.config.dimension.get())
                * std::mem::size_of::<f32>() as u64,
            trash_bytes: store.trash_bytes,
            query_latency: self.table.latency_histogram(),
            per_namespace,
            quant: QuantStat {
                configured: self.config.quantization,
                active: self.config.quantization,
                recall_est: None,
            },
            compaction: self.control.state(),
            retain: self.table.retain_report(),
            relations,
            history: HistoryStat {
                retained_versions: view.versions.values().map(|chain| chain.len() as u64).sum(),
                reclaimed_versions: view.reclaimed_versions,
                horizon: self.config.compaction.history_horizon,
            },
            storage: StorageStat {
                encryption: false,
                compression: self.config.compression,
                migrated_segments: 0,
                total_segments: store.total_segments,
            },
        })
    }

    /// 收集持久后端的段/WAL/trash 统计;纯内存库返回零值。
    fn collect_store_stats(&self, view: &ReaderView) -> StoreStats {
        match &self.store {
            Some(store) => StoreStats {
                segments: store.segment_stats(&view.indexes),
                wal_bytes: store.wal_bytes(),
                trash_bytes: store.trash_bytes(),
                total_segments: store.total_segments(),
            },
            None => StoreStats::default(),
        }
    }

    /// fsck:校验内部索引一致性与持久段完整性,并给出运维建议。
    ///
    /// 内存侧:仅当 key 索引指向不存在的物理版本,或最新版本 `ns_id`/`key` 与索引
    /// 不符时报告不一致;已删除(墓碑)与已逻辑过期的记录不算不一致(FC-MEM-POST-008)。
    /// 持久侧(L2):逐段校验头部/payload CRC 与版本链记录体(设计 16 §1.6)。
    /// 另报告每段墓碑/过期占比与合并建议(FC-LIFE-POST-009;建议不影响 `ok`)。
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

impl Mneme {
    /// 返回后台合并控制句柄(与库共享同一状态)。
    ///
    /// # Returns
    /// 与库共享同一合并状态的 [`CompactionControl`]。
    pub fn compact_control(&self) -> CompactionControl {
        self.control.clone()
    }

    /// 显式落盘:把未落盘槽位与自上次 flush 的 delta 物化为新段并提交 MANIFEST。
    ///
    /// 纯内存库为空操作;持久库执行增量段 flush + WAL Checkpoint(设计 04 §3.2、
    /// 07 §4);无新增且无 delta 时为空操作。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`];只读模式返回
    /// [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub fn flush(&self) -> Result<()> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        drop(view);
        if let Some(store) = &self.store {
            let mut ws = self.table.write();
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            store.flush(&mut ws, &self.config)?;
            self.table.publish(&ws);
        }
        Ok(())
    }

    /// 显式执行一轮 size-tiered compaction(设计 07 §4)。
    ///
    /// 选段与幸存版本筛选由 L5 完成;合并段写盘与 MANIFEST 替换由持久层完成。
    /// 无触发条件、已暂停或纯内存库时为空操作。合并期间 `stats().compaction`
    /// 反映 `Running`(运行中暂停则为 `Paused`);`pause()` 在提交前生效,中止时
    /// 不改动任何已提交状态。
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
    fn run_compaction(
        &self,
        ws: &mut WriterState,
        store: &Arc<crate::persist::store::Store>,
    ) -> Result<()> {
        let now_ms = self.config.clock.now_unix_ms();
        let Some(plan) = self.plan_compaction(ws, store, now_ms) else {
            return Ok(());
        };
        self.control
            .mark_running(plan.segments.iter().map(|id| SegmentId::new(*id)).collect());
        if self.control.is_paused() {
            self.control.mark_idle();
            return Ok(());
        }
        let survivors =
            compact::select_survivors(ws, &plan, now_ms, self.config.compaction.history_horizon);
        let input = crate::persist::store::CompactInput {
            plan: &plan,
            keep_slots: &survivors.keep,
            control: &self.control,
        };
        match store.compact(ws, &self.config, &input) {
            Ok(false) => {
                // 暂停中止:段集与内存状态都不动。
                self.control.mark_idle();
                Ok(())
            }
            Ok(true) => {
                self.finish_compaction(ws, &survivors);
                Ok(())
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
