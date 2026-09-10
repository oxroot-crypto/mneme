//! 内存引擎门面 `Mneme`(`engine.rs`)。
//!
//! 承载库句柄 `Mneme` 及其生命周期/统计/快照门面方法;公开签名在 L1 冻结
//! (设计 03 §8),后续层只替换实现。写入与检索的公开 API 见
//! [`Namespace`](crate::memory::Namespace)。

use std::path::Path;
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::types::{NsId, RowId};
use crate::memory::builder::Builder;
use crate::memory::config::Config;
use crate::memory::namespace::Namespace;
use crate::memory::ops::{BackupReport, CompactionControl};
use crate::memory::snapshot::SnapshotHandle;
use crate::memory::table::Table;
use crate::memory::temporal;
use crate::persist::store::Store;

/// 库句柄;内部 `Arc` 共享,克隆廉价且可跨线程。
#[derive(Clone)]
pub struct Mneme {
    pub(crate) table: Arc<Table>,
    pub(crate) config: Arc<Config>,
    pub(crate) control: CompactionControl,
    /// 持久层协调句柄;纯内存库为 `None`。
    pub(crate) store: Option<Arc<Store>>,
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
    /// ```no_run
    /// use mneme::Mneme;
    /// // 仅编译不执行:会打开/创建本地目录。
    /// let db = Mneme::open("data/agent_memory").expect("open");
    /// # let _ = db;
    /// ```
    pub fn open(path: impl AsRef<Path>) -> Result<Mneme> {
        Builder::default().path(path).build()
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
        let config = Arc::clone(&self.config);
        self.table.write_tx(move |ws| {
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
            let now = config.clock.now_unix_ms();
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
            Ok(victims.len())
        })
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
        if let Some(store) = &self.store {
            let ws = self.table.write();
            // 只读库不写盘;可写库在关闭前把全部已确认写入落成段(I16)。
            if !ws.closed && !self.config.read_only {
                store.flush(&ws, &self.config)?;
            }
            drop(ws);
            store.release_lock();
        }
        let table = Arc::clone(&self.table);
        table.write_tx(move |ws| {
            ws.closed = true;
            Ok(())
        })
    }
}
