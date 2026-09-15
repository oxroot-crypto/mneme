//! 运行统计:命名空间活行聚合、墓碑占比与量化状态汇总。

use std::collections::HashMap;

use crate::core::error::{MnemeError, Result};
use crate::core::options::VectorFormat;
use crate::memory::engine::Mneme;
use crate::memory::ops::{HistoryStat, NsStat, QuantStat, SegmentStat, Stats, StorageStat};
use crate::memory::table::ReaderView;

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
pub(super) fn dead_ratios(view: &ReaderView, now: i64) -> HashMap<u32, f32> {
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

/// 汇总量化运行状态:`active` 取实际存在副本的格式(无副本 = `F32`,绝不回显
/// 配置);`recall_est` 取各段建段抽样估计的最小值(重开库后为 `None`,I13)。
///
/// 多段格式混杂(如旧 i8 段 + 新 f16 段)时 `active` 取**首个非 `F32` 段**的
/// 格式(按段序),单枚举无法同时表达两种生效格式;逐段真实格式以
/// `stats().segments[*].quant` 为准。
fn quant_stat(configured: VectorFormat, segments: &[SegmentStat]) -> QuantStat {
    let mut active = VectorFormat::F32;
    let mut recall_est: Option<f32> = None;
    for segment in segments {
        if segment.quant == VectorFormat::F32 {
            continue;
        }
        if active == VectorFormat::F32 {
            active = segment.quant;
        }
        if let Some(estimate) = segment.recall_est {
            recall_est = Some(recall_est.map_or(estimate, |current| current.min(estimate)));
        }
    }
    QuantStat {
        configured,
        active,
        recall_est,
    }
}

impl Mneme {
    /// 返回运行统计。
    ///
    /// # Returns
    /// 当前视图的运行统计快照,字段见 [`Stats`]。
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
        let quant = quant_stat(self.config.quantization, &store.segments);
        Ok(Stats {
            segments: store.segments,
            wal_bytes: store.wal_bytes,
            memory_est: live_rows
                * u64::from(self.config.dimension.get())
                * std::mem::size_of::<f32>() as u64,
            trash_bytes: store.trash_bytes,
            query_latency: self.table.latency_histogram(),
            per_namespace,
            quant,
            compaction: self.control.state(),
            retain: self.table.retain_report(),
            relations,
            history: HistoryStat {
                retained_versions: view.versions.values().map(|chain| chain.len() as u64).sum(),
                reclaimed_versions: view.reclaimed_versions,
                horizon: self.config.compaction.history_horizon,
            },
            storage: StorageStat {
                encryption: self
                    .store
                    .as_ref()
                    .is_some_and(|store| store.encryption_enabled()),
                compression: self.config.compression,
                migrated_segments: self
                    .store
                    .as_ref()
                    .map_or(0, |store| store.migrated_segments()),
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
}
