//! 快照句柄 [`SnapshotHandle`]:钉住某个读视图。

use std::sync::Arc;

use crate::memory::table::{ReaderView, Table};

use super::namespace::SnapshotNamespace;

/// 快照句柄:钉住某个读视图。
#[derive(Clone)]
pub struct SnapshotHandle {
    pub(crate) table: Arc<Table>,
    pub(crate) view: Arc<ReaderView>,
    pub(crate) as_of_ms: i64,
}

impl std::fmt::Debug for SnapshotHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotHandle")
            .field("version", &self.view.seqno)
            .field("as_of_ms", &self.as_of_ms)
            .finish_non_exhaustive()
    }
}

impl SnapshotHandle {
    /// 构建该视图时的基线序号水位。
    ///
    /// # Returns
    ///
    /// 快照视图的基线 [`SeqNo`](crate::SeqNo) 原始值;钉住后不随后续写入变化。
    pub fn version(&self) -> u64 {
        self.view.seqno.get()
    }

    /// 事务时间上界(普通快照 = 当前,`as_of` = 指定时刻)。
    ///
    /// # Returns
    ///
    /// 事务时间上界(Unix 毫秒):普通快照为构建时刻,`as_of` 快照为调用方
    /// 指定的 `ts_ms`。
    pub fn as_of_ms(&self) -> i64 {
        self.as_of_ms
    }

    /// 返回钉住视图的只读统计(段数/物理行数/基线水位)。
    ///
    /// 统计取自快照钉住的 `ReaderView`,不随后续写入或后台 compaction 变化
    /// (设计 07 §6、I17)。
    ///
    /// # Returns
    ///
    /// 钉住视图的 [`SnapshotStats`](crate::memory::ops::SnapshotStats):基线水位、
    /// 活跃段数与物理槽位数(含历史版本与墓碑,已物理回收槽位不计入)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.namespace("demo")
    ///     .insert(Record::new(vec![1.0, 0.0]).key("a"))
    ///     .unwrap();
    /// let stats = db.snapshot().stats();
    /// assert_eq!(stats.rows, 1);
    /// ```
    pub fn stats(&self) -> crate::memory::ops::SnapshotStats {
        let mut segments = std::collections::HashSet::new();
        let mut rows = 0_u64;
        for (index, segment) in self.view.slot_segment.iter().enumerate() {
            // 已被物理回收(dead)的槽位不属于任何活跃段,不计入统计。
            if self.view.dead.get(index) {
                continue;
            }
            rows += 1;
            if let Some(id) = segment {
                segments.insert(*id);
            }
        }
        crate::memory::ops::SnapshotStats {
            version: self.version(),
            segments: segments.len(),
            rows,
        }
    }

    /// 在钉住的快照上取命名空间只读视图。
    ///
    /// # Arguments
    /// * `path` - 命名空间路径,按 `/` 分层;不校验是否已注册。
    ///
    /// # Returns
    /// 指向 `path` 的 [`SnapshotNamespace`];不校验路径是否已注册。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// let snap = db.snapshot().namespace("demo");
    /// assert!(snap.get("a").unwrap().is_some());
    /// ```
    pub fn namespace(&self, path: &str) -> SnapshotNamespace {
        SnapshotNamespace {
            table: Arc::clone(&self.table),
            view: Arc::clone(&self.view),
            ns_path: Arc::from(crate::memory::namespace::normalize_path(path)),
            as_of_ms: self.as_of_ms,
        }
    }
}
