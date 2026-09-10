//! 内存引擎门面 `Mneme`(`engine.rs`)。
//!
//! 承载库句柄 `Mneme` 及其生命周期/统计/快照门面方法;公开签名在 L1 冻结
//! (设计 03 §8),后续层只替换实现。写入与检索的公开 API 见
//! [`Namespace`](crate::memory::Namespace)。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::types::{NsId, RowId};
use crate::memory::builder::Builder;
use crate::memory::config::Config;
use crate::memory::namespace::Namespace;
use crate::memory::ops::{
    BackupReport, CheckReport, CompactionControl, Histogram, HistoryStat, NsStat, QuantStat, Stats,
    StorageStat,
};
use crate::memory::snapshot::SnapshotHandle;
use crate::memory::table::Table;
use crate::memory::temporal;

/// 库句柄;内部 `Arc` 共享,克隆廉价且可跨线程。
#[derive(Clone)]
pub struct Mneme {
    pub(crate) table: Arc<Table>,
    pub(crate) config: Arc<Config>,
    pub(crate) control: CompactionControl,
}

impl std::fmt::Debug for Mneme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mneme")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Mneme {
    /// 打开一个已初始化的本地目录记忆库(持久化在 L2 实现)。
    ///
    /// # Arguments
    /// * `_path` - 已初始化库的目录路径;L1 阶段忽略,恒返回 `Unsupported`,
    ///   L2 持久层落地后生效。
    ///
    /// # Errors
    /// 当前恒返回 [`MnemeError::Unsupported`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::Mneme;
    /// assert!(Mneme::open("some/dir").is_err());
    /// ```
    pub fn open(_path: impl AsRef<Path>) -> Result<Mneme> {
        Err(MnemeError::Unsupported {
            feature: "持久化(open, L2)",
        })
    }

    /// 创建纯内存库(易失),维度必填。
    ///
    /// # Arguments
    /// * `dimension` - 向量维度;取值 `[1, 65536]`,建库后锁定不可变。
    ///
    /// # Errors
    /// 维度超出 `[1, 65536]` 时返回 [`MnemeError::LimitExceeded`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::Mneme;
    /// let db = Mneme::in_memory(3).unwrap();
    /// assert!(db.namespace("demo").get("missing").unwrap().is_none());
    /// ```
    pub fn in_memory(dimension: u32) -> Result<Mneme> {
        Builder::default().dimension(dimension).build()
    }

    /// 返回建库器。
    ///
    /// # Returns
    /// 全默认配置的 [`Builder`]。
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// 返回命名空间句柄;注册表在首次成功写入时惰性登记。
    ///
    /// # Arguments
    /// * `path` - 命名空间路径,按 `/` 分层(如 `"project/session"`),
    ///   前缀关系供 `drop_namespace` 级联删除;不校验是否已注册。
    ///
    /// # Returns
    /// 指向 `path` 的命名空间句柄;不校验路径是否已注册。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// assert!(ns.get("a").unwrap().is_some());
    /// ```
    pub fn namespace(&self, path: &str) -> Namespace {
        Namespace {
            table: Arc::clone(&self.table),
            config: Arc::clone(&self.config),
            ns_path: Arc::from(path),
        }
    }

    /// 列出已注册的命名空间路径(字典序,即前缀树顺序)。
    ///
    /// # Returns
    /// 已注册命名空间路径的字典序列表。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.namespace("demo").insert(Record::new(vec![1.0, 0.0])).unwrap();
    /// assert_eq!(db.list_namespaces().unwrap(), vec!["demo"]);
    /// ```
    pub fn list_namespaces(&self) -> Result<Vec<String>> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let mut paths: Vec<String> = view
            .ns_registry
            .values()
            .map(|path| path.to_string())
            .collect();
        paths.sort();
        Ok(paths)
    }

    /// 删除命名空间及其所有子命名空间,返回删除的命名空间数。
    ///
    /// # Arguments
    /// * `path` - 要删除的命名空间路径;精确匹配该路径及全部 `path/` 前缀子命名空间。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`];槽位编号溢出 `u32::MAX` 时返回
    /// [`MnemeError::LimitExceeded`]——仅超大规模库(槽位数 > `u32::MAX`)可达。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.namespace("demo").insert(Record::new(vec![1.0, 0.0])).unwrap();
    /// assert_eq!(db.drop_namespace("demo").unwrap(), 1);
    /// ```
    pub fn drop_namespace(&self, path: &str) -> Result<usize> {
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        let prefix = format!("{path}/");
        let victims: Vec<(NsId, Arc<str>)> = ws
            .ns_registry
            .iter()
            .filter(|(_, registered)| {
                let registered: &str = registered;
                registered == path || registered.starts_with(&prefix)
            })
            .map(|(id, registered)| (*id, Arc::clone(registered)))
            .collect();
        let now = self.config.clock.now_unix_ms();
        for (ns_id, _) in &victims {
            let rowids: Vec<RowId> = ws
                .slots
                .iter()
                .filter(|slot| slot.ns_id == *ns_id && !slot.deleted)
                .map(|slot| slot.rowid)
                .collect();
            for rowid in rowids {
                let seqno = ws.alloc_seqno();
                ws.tombstone(rowid, now, seqno)?;
            }
            Arc::make_mut(&mut ws.ns_registry).remove(ns_id);
        }
        for (_, path) in &victims {
            Arc::make_mut(&mut ws.ns_by_path).remove(path);
        }
        let count = victims.len();
        self.table.publish(&ws);
        Ok(count)
    }

    /// 钉住当前读视图,返回一致快照句柄。
    ///
    /// # Returns
    /// 钉住当前读视图的 [`SnapshotHandle`],`as_of_ms` 为当前时刻。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.namespace("demo")
    ///     .insert(Record::new(vec![1.0, 0.0]))
    ///     .unwrap();
    /// let snap = db.snapshot();
    /// assert!(snap.version() >= 1);
    /// ```
    pub fn snapshot(&self) -> SnapshotHandle {
        SnapshotHandle {
            table: Arc::clone(&self.table),
            view: self.table.view(),
            as_of_ms: self.config.clock.now_unix_ms(),
        }
    }

    /// 双时态历史读:取事务时间 ≤ `ts_ms` 的可见版本组成一致快照。
    ///
    /// # Arguments
    /// * `ts_ms` - 事务时间上界(Unix 毫秒);仅包含 `tx_ms <= ts_ms` 的版本。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::Mneme;
    /// let db = Mneme::in_memory(2).unwrap();
    /// let snap = db.as_of(0).unwrap();
    /// assert_eq!(snap.as_of_ms(), 0);
    /// ```
    pub fn as_of(&self, ts_ms: i64) -> Result<SnapshotHandle> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let snap = temporal::snapshot_at(&view, ts_ms);
        Ok(SnapshotHandle {
            table: Arc::clone(&self.table),
            view: Arc::new(snap),
            as_of_ms: ts_ms,
        })
    }

    /// 备份到目录(持久化在 L2 实现)。
    ///
    /// # Arguments
    /// * `_dir` - 备份目标目录;L1 阶段忽略,恒返回 `Unsupported`。
    ///
    /// # Errors
    /// 当前恒返回 [`MnemeError::Unsupported`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::Mneme;
    /// let db = Mneme::in_memory(2).unwrap();
    /// assert!(db.backup_to("backup").is_err());
    /// ```
    pub fn backup_to(&self, _dir: impl AsRef<Path>) -> Result<BackupReport> {
        Err(MnemeError::Unsupported {
            feature: "备份(backup_to, L2)",
        })
    }

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
        for (idx, slot) in view.slots.iter().enumerate() {
            if view.dead.get(idx) || slot.deleted {
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
    /// ```
    pub fn check(&self) -> Result<CheckReport> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        let mut suggestions = Vec::new();
        for ((ns_id, key), rowid) in view.key_index.iter() {
            match view.live_slot(*rowid) {
                Some(slot) => {
                    let slot_data = &view.slots[slot.get() as usize];
                    if slot_data.ns_id != *ns_id || slot_data.key.as_ref() != Some(key) {
                        suggestions.push(format!("key 索引不一致:{key}"));
                    }
                }
                None => suggestions.push(format!("key 索引指向不可见行:{key}")),
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

    /// 显式落盘(L1 无持久化,为空操作)。
    ///
    /// # Returns
    /// 恒 `Ok`(L1 无持久化,无 I/O)。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    pub fn flush(&self) -> Result<()> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        Ok(())
    }

    /// 关闭共享库:标记关闭并释放资源;幂等。
    ///
    /// # Errors
    /// 恒 `Ok`(L1 关闭不产生 I/O;`Result` 为 L2 持久化错误预留)。
    ///
    /// # Examples
    /// ```
    /// use mneme::Mneme;
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.close().unwrap();
    /// ```
    pub fn close(self) -> Result<()> {
        let mut ws = self.table.write();
        ws.closed = true;
        self.table.publish(&ws);
        Ok(())
    }
}
