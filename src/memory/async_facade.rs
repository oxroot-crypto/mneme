//! async 门面 `AsyncNamespace`(feature `async`;设计 08 §6)。
//!
//! 核心库零 tokio:所有 async 方法都是 `spawn_blocking(同步方法)` 的机械包装,
//! 与同步 API 同语义(I14)——共享同一底层句柄与写锁,同一操作序列产生等价结果。
//! `search()` 构建器本身是轻量纯内存操作,`execute()` 为阻塞调用,异步场景请由
//! 宿主对 `execute()` 自行 `spawn_blocking`;`Mneme` 级操作(flush/close/backup/
//! snapshot)亦不走本门面。
//!
//! 借用返回的同步方法(`iter`/`iter_with`)不在此门面:其迭代器借自读视图,
//! 无法跨线程移动;`get` 一类返回 [`RecordRef`](crate::RecordRef) 的方法改为
//! 返回 owned [`Record`](crate::Record),语义等价。

use crate::core::error::Result;
use crate::core::options::{Feedback, QueryId, RelationKind, UpdatePatch};
use crate::core::types::RowId;
use crate::memory::lifecycle::{RetainReport, Retention};
use crate::memory::namespace::Namespace;
use crate::memory::pred::Expr;
use crate::memory::record::{InsertOutcome, Record, StoredRecord, UpdateOutcome};
use crate::memory::relation::{Edge, RelateOptions};
use crate::memory::score::{ConsolidateReport, ConsolidationPolicy};

/// 把阻塞调用移入 tokio 阻塞线程池。
///
/// # Panics
///
/// 阻塞池任务异常终止(取消/panic)时 panic;按设计 16 §4 的文档化例外传播——
/// 核心库承诺不 panic,出现即属运行时故障,绝不静默吞掉。
async fn run_blocking<T, F>(task: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        // reason: 核心库不 panic(FC-GLOBAL-ERR-001);阻塞任务异常终止属运行时故障,
        // 按设计 16 §4 的文档化 panic 例外传播(同 `filter!` 宏的文档化例外)。
        .expect("async 门面:阻塞任务异常终止")
}

/// [`Namespace`] 的异步门面(feature `async`)。
///
/// 轻量包装类型:与同步 `Namespace` 共享同一底层句柄与写锁,所有方法等价于
/// `spawn_blocking` 包装的同步调用(I14)。经 [`Namespace::into_async`] 构造;
/// 本类型 `Send + Sync`。
#[derive(Debug, Clone)]
pub struct AsyncNamespace {
    /// 被包装的同步命名空间句柄。
    inner: Namespace,
}

impl Namespace {
    /// 转为异步门面(共享同一句柄与写锁)。
    ///
    /// # Returns
    ///
    /// 包装本句柄的 [`AsyncNamespace`]。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mneme::{Mneme, Record};
    ///
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo").into_async();
    /// let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    /// runtime.block_on(async {
    ///     ns.insert(Record::new(vec![1.0, 0.0])).await.unwrap();
    /// });
    /// ```
    pub fn into_async(self) -> AsyncNamespace {
        AsyncNamespace { inner: self }
    }
}

impl AsyncNamespace {
    /// 插入一条记录;等价于同步 [`Namespace::insert`]。
    ///
    /// # Arguments
    /// * `rec` - 待插入记录(维度必须与建库维度一致)。
    ///
    /// # Returns
    /// `Ok(InsertOutcome::Inserted(rowid))`(去重策略可能返回合并/拒绝语义)。
    ///
    /// # Errors
    /// 同同步 API:库已关闭、维度/限额/非有限值校验失败、`Dedup` 策略冲突等。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mneme::{Mneme, Record};
    ///
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo").into_async();
    /// let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    /// runtime.block_on(async {
    ///     ns.insert(Record::new(vec![1.0, 0.0]).key("a")).await.unwrap();
    /// });
    /// ```
    pub async fn insert(&self, rec: Record) -> Result<InsertOutcome> {
        let inner = self.inner.clone();
        run_blocking(move || inner.insert(rec)).await
    }

    /// 批量插入;等价于同步 [`Namespace::insert_batch`](crate::Namespace::insert_batch)。
    ///
    /// # Errors
    /// 任一记录校验失败时整批回滚(零部分写入),不产生任何可见变更。
    pub async fn insert_batch(&self, recs: Vec<Record>) -> Result<Vec<InsertOutcome>> {
        let inner = self.inner.clone();
        run_blocking(move || inner.insert_batch(recs)).await
    }

    /// 按 key 删除;等价于同步 [`Namespace::delete`](crate::Namespace::delete)。
    ///
    /// # Returns
    /// 删除了可见记录返回 `true`;key 不存在或已墓碑返回 `false`。
    pub async fn delete(&self, key: &str) -> Result<bool> {
        let inner = self.inner.clone();
        let key = key.to_string();
        run_blocking(move || inner.delete(&key)).await
    }

    /// 按 `RowId` 删除;等价于同步 [`Namespace::delete_by_rowid`](crate::Namespace::delete_by_rowid)。
    pub async fn delete_by_rowid(&self, id: RowId) -> Result<bool> {
        let inner = self.inner.clone();
        run_blocking(move || inner.delete_by_rowid(id)).await
    }

    /// 按 key 局部更新;等价于同步 [`Namespace::update`](crate::Namespace::update)。
    pub async fn update(&self, key: &str, patch: UpdatePatch) -> Result<UpdateOutcome> {
        let inner = self.inner.clone();
        let key = key.to_string();
        run_blocking(move || inner.update(&key, patch)).await
    }

    /// 按 `RowId` 局部更新;等价于同步 [`Namespace::update_by_rowid`](crate::Namespace::update_by_rowid)。
    pub async fn update_by_rowid(&self, id: RowId, patch: UpdatePatch) -> Result<UpdateOutcome> {
        let inner = self.inner.clone();
        run_blocking(move || inner.update_by_rowid(id, patch)).await
    }

    /// 信念修订;等价于同步 [`Namespace::supersede`](crate::Namespace::supersede)。
    pub async fn supersede(&self, key: &str, rec: Record) -> Result<UpdateOutcome> {
        let inner = self.inner.clone();
        let key = key.to_string();
        run_blocking(move || inner.supersede(&key, rec)).await
    }

    /// 按 key 点读;返回 owned [`StoredRecord`](crate::StoredRecord)
    /// (同步版返回 [`RecordRef`](crate::RecordRef))。
    ///
    /// # Returns
    /// 可见记录返回 `Some(StoredRecord)`;不存在/墓碑/逻辑过期返回 `None`。
    pub async fn get(&self, key: &str) -> Result<Option<StoredRecord>> {
        let inner = self.inner.clone();
        let key = key.to_string();
        run_blocking(move || {
            inner
                .get(&key)
                .map(|found| found.map(|record| record.to_stored()))
        })
        .await
    }

    /// 按 `RowId` 点读;返回 owned [`StoredRecord`](crate::StoredRecord)。
    pub async fn get_by_rowid(&self, id: RowId) -> Result<Option<StoredRecord>> {
        let inner = self.inner.clone();
        run_blocking(move || {
            inner
                .get_by_rowid(id)
                .map(|found| found.map(|record| record.to_stored()))
        })
        .await
    }

    /// 批量点读(顺序保持);返回 owned [`StoredRecord`](crate::StoredRecord) 列表。
    pub async fn get_many(&self, keys: &[&str]) -> Result<Vec<Option<StoredRecord>>> {
        let inner = self.inner.clone();
        let keys: Vec<String> = keys.iter().map(|key| (*key).to_string()).collect();
        run_blocking(move || {
            let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
            inner.get_many(&refs).map(|list| {
                list.into_iter()
                    .map(|found| found.map(|record| record.to_stored()))
                    .collect()
            })
        })
        .await
    }

    /// 按 `RowId` 批量点读(顺序保持);返回 owned
    /// [`StoredRecord`](crate::StoredRecord) 列表。
    pub async fn get_many_by_rowid(&self, ids: &[RowId]) -> Result<Vec<Option<StoredRecord>>> {
        let inner = self.inner.clone();
        let ids = ids.to_vec();
        run_blocking(move || {
            inner.get_many_by_rowid(&ids).map(|list| {
                list.into_iter()
                    .map(|found| found.map(|record| record.to_stored()))
                    .collect()
            })
        })
        .await
    }

    /// 判断 key 是否存在可见记录;等价于同步 [`Namespace::exists`](crate::Namespace::exists)。
    pub async fn exists(&self, key: &str) -> Result<bool> {
        let inner = self.inner.clone();
        let key = key.to_string();
        run_blocking(move || inner.exists(&key)).await
    }

    /// 取向量副本;等价于同步 [`Namespace::get_vector`](crate::Namespace::get_vector)。
    ///
    /// # Returns
    /// 记录存在时返回 `Some(f32 向量)`;否则 `None`。
    pub async fn get_vector(&self, id: RowId) -> Result<Option<Vec<f32>>> {
        let inner = self.inner.clone();
        run_blocking(move || inner.get_vector(id)).await
    }

    /// 计数(可选过滤);等价于同步 [`Namespace::count`](crate::Namespace::count)。
    pub async fn count(&self, filter: Option<Expr>) -> Result<u64> {
        let inner = self.inner.clone();
        run_blocking(move || inner.count(filter)).await
    }

    /// 按 key 记一次访问;等价于同步 [`Namespace::touch`](crate::Namespace::touch)。
    ///
    /// # Arguments
    /// * `key` - 目标 key。
    /// * `boost` - 重要度增量;`None` 表示仅累加访问计数。
    ///
    /// # Returns
    /// 命中可见记录返回 `true`;否则 `false`。
    pub async fn touch(&self, key: &str, boost: Option<f32>) -> Result<bool> {
        let inner = self.inner.clone();
        let key = key.to_string();
        run_blocking(move || inner.touch(&key, boost)).await
    }

    /// 按 `RowId` 记一次访问;等价于同步 [`Namespace::touch_by_rowid`](crate::Namespace::touch_by_rowid)。
    pub async fn touch_by_rowid(&self, id: RowId, boost: Option<f32>) -> Result<bool> {
        let inner = self.inner.clone();
        run_blocking(move || inner.touch_by_rowid(id, boost)).await
    }

    /// 记录用户反馈;等价于同步 [`Namespace::feedback`](crate::Namespace::feedback)。
    ///
    /// # Returns
    /// 首次生效返回 `true`;记录不可见或同 `(RowId, QueryId)` 幂等键重复返回 `false`。
    pub async fn feedback(&self, id: RowId, feedback: Feedback, query_id: QueryId) -> Result<bool> {
        let inner = self.inner.clone();
        run_blocking(move || inner.feedback(id, feedback, query_id)).await
    }

    /// 建立带权关系边;等价于同步 [`Namespace::relate`](crate::Namespace::relate)。
    ///
    /// # Arguments
    /// * `from`、`to` - 边的源/目标 `RowId`。
    /// * `kind` - 关系类型。
    /// * `weight` - 边权,入口钳制到 `[0.0, 1.0]`。
    pub async fn relate(
        &self,
        from: RowId,
        to: RowId,
        kind: RelationKind,
        weight: f32,
    ) -> Result<()> {
        let inner = self.inner.clone();
        run_blocking(move || inner.relate(from, to, kind, weight)).await
    }

    /// 建立带选项的关系边;等价于同步
    /// [`Namespace::relate_with_options`](crate::Namespace::relate_with_options)。
    pub async fn relate_with_options(
        &self,
        from: RowId,
        to: RowId,
        options: RelateOptions,
    ) -> Result<()> {
        let inner = self.inner.clone();
        run_blocking(move || inner.relate_with_options(from, to, options)).await
    }

    /// 删除关系边;等价于同步 [`Namespace::unrelate`](crate::Namespace::unrelate)。
    ///
    /// # Returns
    /// 边原先存在返回 `true`;否则 `false`。
    pub async fn unrelate(&self, from: RowId, to: RowId, kind: RelationKind) -> Result<bool> {
        let inner = self.inner.clone();
        run_blocking(move || inner.unrelate(from, to, kind)).await
    }

    /// 列出出边;等价于同步 [`Namespace::neighbors`](crate::Namespace::neighbors)。
    pub async fn neighbors(&self, from: RowId, kinds: &[RelationKind]) -> Result<Vec<Edge>> {
        let inner = self.inner.clone();
        let kinds = kinds.to_vec();
        run_blocking(move || inner.neighbors(from, &kinds)).await
    }

    /// 列出入边;等价于同步 [`Namespace::predecessors`](crate::Namespace::predecessors)。
    pub async fn predecessors(&self, to: RowId, kinds: &[RelationKind]) -> Result<Vec<Edge>> {
        let inner = self.inner.clone();
        let kinds = kinds.to_vec();
        run_blocking(move || inner.predecessors(to, &kinds)).await
    }

    /// 按过滤条件遗忘;等价于同步 [`Namespace::forget`](crate::Namespace::forget)。
    ///
    /// # Returns
    /// 被遗忘的记录数。
    pub async fn forget(&self, filter: Expr) -> Result<usize> {
        let inner = self.inner.clone();
        run_blocking(move || inner.forget(filter)).await
    }

    /// 按遗忘策略回收;等价于同步 [`Namespace::retain`](crate::Namespace::retain)。
    pub async fn retain(&self, policy: Retention) -> Result<RetainReport> {
        let inner = self.inner.clone();
        run_blocking(move || inner.retain(policy)).await
    }

    /// 记忆沉淀;等价于同步 [`Namespace::consolidate`](crate::Namespace::consolidate)。
    pub async fn consolidate(&self, policy: ConsolidationPolicy) -> Result<ConsolidateReport> {
        let inner = self.inner.clone();
        run_blocking(move || inner.consolidate(policy)).await
    }
}
