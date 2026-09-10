//! 内存引擎的统计与自检门面(`engine_ops.rs`)。
//!
//! 以 `impl Mneme` 扩展 [`Mneme`](crate::memory::engine::Mneme) 的运维方法
//! (运行统计、fsck、合并控制、落盘空操作),与库生命周期方法分置于不同文件。

use std::collections::HashMap;

use crate::core::error::{MnemeError, Result};
use crate::memory::engine::Mneme;
use crate::memory::ops::{
    CheckReport, CompactionControl, Histogram, HistoryStat, NsStat, QuantStat, Stats, StorageStat,
};

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
        let mut per_namespace: HashMap<String, NsStat> = HashMap::new();
        let mut live_rows = 0_u64;
        let now = self.config.clock.now_unix_ms();
        for (idx, slot) in view.slots.iter().enumerate() {
            // 逻辑过期记录与墓碑一样不计入统计(FC-LIFE-INV-009)。
            if view.dead.get(idx) || !slot.is_live(now) {
                continue;
            }
            live_rows += 1;
            let stat = per_namespace.entry(slot.ns_path.to_string()).or_default();
            stat.doc_count += 1;
            stat.total_doc_len += slot.text.as_ref().map_or(0, |text| text.len() as u64);
        }
        let relations = view.out_edges.values().map(Vec::len).sum::<usize>() as u64;
        Ok(Stats {
            segments: Vec::new(),
            wal_bytes: 0,
            memory_est: live_rows
                * u64::from(self.config.dimension.get())
                * std::mem::size_of::<f32>() as u64,
            trash_bytes: 0,
            query_latency: Histogram::default(),
            per_namespace,
            quant: QuantStat {
                configured: self.config.quantization,
                active: self.config.quantization,
                recall_est: None,
            },
            compaction: self.control.state(),
            retain: None,
            relations,
            history: HistoryStat {
                retained_versions: view.slots.len() as u64,
                reclaimed_versions: 0,
                horizon: self.config.compaction.history_horizon,
            },
            storage: StorageStat {
                encryption: false,
                compression: self.config.compression,
                migrated_segments: 0,
                total_segments: 0,
            },
        })
    }

    /// fsck:校验内部索引一致性。
    ///
    /// 仅当 key 索引指向不存在的物理版本,或最新版本 `ns_id`/`key` 与索引不符时
    /// 报告不一致;已删除(墓碑)与已逻辑过期的记录不算不一致(FC-MEM-POST-008)。
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
        let mut suggestions = Vec::new();
        for ((ns_id, key), rowid) in view.key_index.iter() {
            // 按最新物理版本对账(墓碑/逻辑过期不算不一致):key 索引指向不存在的
            // 版本,或最新版本 ns/key 与索引不符,才是真正的不一致(FC-MEM-POST-008)。
            match view.latest.get(rowid) {
                Some(slot) => {
                    let slot_data = &view.slots[slot.get() as usize];
                    if slot_data.ns_id != *ns_id || slot_data.key.as_ref() != Some(key) {
                        suggestions.push(format!("key 索引不一致:{key}"));
                    }
                }
                None => suggestions.push(format!("key 索引指向不存在的版本:{key}")),
            }
        }
        Ok(CheckReport {
            ok: suggestions.is_empty(),
            corrupted: Vec::new(),
            suggestions,
        })
    }

    /// 返回后台合并控制句柄(与库共享同一状态)。
    ///
    /// # Returns
    /// 与库共享同一合并状态的 [`CompactionControl`]。
    pub fn compact_control(&self) -> CompactionControl {
        self.control.clone()
    }

    /// 显式落盘:把可变表物化为新段并提交 MANIFEST。
    ///
    /// 纯内存库为空操作;持久库执行全量快照 flush 并重置 WAL(设计 04 §3.2)。
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
            let ws = self.table.write();
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            store.flush(&ws, &self.config)?;
        }
        Ok(())
    }
}
