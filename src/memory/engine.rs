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
use crate::memory::table::{Table, WriterState};
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
    /// 后台维护线程句柄(读路径命中攒批 / 自动遗忘 / 自动 compaction)。
    pub(crate) maintenance: Option<crate::life::maintenance::MaintenanceHandle>,
}

impl std::fmt::Debug for Mneme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mneme")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Drop for Mneme {
    fn drop(&mut self) {
        // 最后一个库句柄释放时同步停掉后台维护,保证独占锁随 `Store` Drop 及时释放;
        // 其它句柄(命名空间/快照)仍存活时线程继续,由弱引用失效自行退出。
        if let Some(maintenance) = &self.maintenance
            && maintenance.is_last_handle()
        {
            maintenance.stop();
        }
    }
}

impl Mneme {
    /// 打开(或按需创建)一个本地目录记忆库(L2 持久化实现)。
    ///
    /// # Arguments
    /// * `path` - 库目录路径;不存在时按默认配置新建(维度需经 [`Mneme::builder`] 指定)。
    ///
    /// # Errors
    /// 目录不可用、锁被占、MANIFEST/段损坏或维度冲突时返回结构化错误。
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
    /// * `path` - 命名空间路径,按 `/` 分层(如 `"project/session"`);自动规范化
    ///   (去首尾 `/`、合并连续 `/`),前缀关系供 `drop_namespace` 级联删除;
    ///   深度/非法字符在首次写入时以 `Config` 报告。
    ///
    /// # Returns
    /// 指向规范化路径的命名空间句柄;不校验路径是否已注册。
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
            ns_path: Arc::from(crate::memory::namespace::normalize_path(path)),
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

    /// 删除命名空间及其所有子命名空间,返回被墓碑的记录行数。
    ///
    /// # Arguments
    /// * `path` - 要删除的命名空间路径(自动规范化);按 `/` 段边界精确匹配该路径
    ///   及全部 `path/` 前缀子命名空间(`"a/b"` 不含 `"a/bc"`);空路径表示根。
    ///
    /// # Returns
    /// 实际打墓碑的记录行数(物理回收留给 compaction)。
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
        let path = crate::memory::namespace::normalize_path(path);
        let config = Arc::clone(&self.config);
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            let prefix = format!("{path}/");
            let victims: Vec<NsId> = ws
                .ns_registry
                .iter()
                .filter(|(_, registered)| {
                    let registered: &str = registered;
                    path.is_empty() || registered == path || registered.starts_with(&prefix)
                })
                .map(|(id, _)| *id)
                .collect();
            let now = config.clock.now_unix_ms();
            let mut rows = 0_usize;
            for ns_id in &victims {
                rows += tombstone_namespace(ws, *ns_id, now)?;
            }
            for ns_id in &victims {
                ws.unregister_ns(*ns_id);
            }
            Ok(rows)
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

    /// 备份到目录:先 `flush`(非只读时)再复制段/MANIFEST/WAL,`current` 最后写。
    ///
    /// 纯内存库无持久内容,返回 [`MnemeError::Unsupported`]。
    ///
    /// # Arguments
    /// * `dir` - 备份目标目录;必须不存在或为空。
    ///
    /// # Errors
    /// 纯内存库返回 [`MnemeError::Unsupported`];目标非空返回 [`MnemeError::Busy`];
    /// I/O 失败返回 [`MnemeError::Io`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::Mneme;
    /// let db = Mneme::in_memory(2).unwrap();
    /// assert!(db.backup_to("backup").is_err());
    /// ```
    pub fn backup_to(&self, dir: impl AsRef<Path>) -> Result<BackupReport> {
        let Some(store) = &self.store else {
            return Err(MnemeError::Unsupported {
                feature: "备份(backup_to, 纯内存库)",
            });
        };
        // 持写锁跨 flush 与复制:阻止并发写触发阈值 flush 把正在复制的段移入 trash,
        // 保证备份的段集合与 MANIFEST 一致(FC-PERSIST-POST-004)。
        let mut ws = self.table.write();
        if ws.closed {
            return Err(MnemeError::Closed);
        }
        if !self.config.read_only {
            store.flush(&mut ws, &self.config)?;
            self.table.publish(&ws);
        }
        store.backup_to(dir.as_ref())
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
        // 先停后台维护,再做最终 flush,避免维护并发写入。
        if let Some(maintenance) = &self.maintenance {
            maintenance.stop();
        }
        if let Some(store) = &self.store {
            let mut ws = self.table.write();
            // 只读库不写盘;可写库在关闭前把全部已确认写入落成段(I16)。
            if !ws.closed && !self.config.read_only {
                store.flush(&mut ws, &self.config)?;
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

/// 为单个命名空间下所有可见记录落墓碑;返回墓碑行数。
///
/// 每个 RowId 只取其最新版本一次:历史版本不进列表,避免重复烧 `seqno` 与对
/// 已墓碑行做无效 tombstone。
fn tombstone_namespace(ws: &mut WriterState, ns_id: NsId, now: i64) -> Result<usize> {
    let rowids: Vec<RowId> = ws
        .latest
        .iter()
        .filter(|(_, slot)| {
            let data = &ws.slots[slot.get() as usize];
            data.ns_id == ns_id && !data.deleted
        })
        .map(|(rowid, _)| *rowid)
        .collect();
    let mut rows = 0;
    for rowid in rowids {
        let seqno = ws.alloc_seqno();
        if ws.tombstone(rowid, now, seqno)? {
            rows += 1;
        }
    }
    Ok(rows)
}
