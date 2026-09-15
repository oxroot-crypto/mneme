//! `AsyncNamespace` 个写入/更新/访问/生命周期方法。

use crate::core::error::Result;
use crate::core::options::{Feedback, QueryId, UpdatePatch};
use crate::core::types::RowId;
// `Namespace` 只出现在 `insert` 个 rustdoc 内链里;代码本体无显式引用,放行告警。
use crate::memory::lifecycle::{RetainReport, Retention};
#[allow(unused_imports)]
use crate::memory::namespace::Namespace;
use crate::memory::pred::Expr;
use crate::memory::record::{InsertOutcome, Record, UpdateOutcome};
use crate::memory::score::{ConsolidateReport, ConsolidationPolicy};

use super::facade::{AsyncNamespace, run_blocking};

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
    /// # Errors
    /// 库已关闭;`key` 校验同同步 API。
    ///
    /// # Returns
    /// 删除了可见记录返回 `true`;key 不存在或已墓碑返回 `false`。
    pub async fn delete(&self, key: &str) -> Result<bool> {
        let inner = self.inner.clone();
        let key = key.to_string();
        run_blocking(move || inner.delete(&key)).await
    }

    /// 按 `RowId` 删除;等价于同步 [`Namespace::delete_by_rowid`](crate::Namespace::delete_by_rowid)。
    ///
    /// # Errors
    /// 库已关闭;`RowId` 校验同同步 API。
    pub async fn delete_by_rowid(&self, id: RowId) -> Result<bool> {
        let inner = self.inner.clone();
        run_blocking(move || inner.delete_by_rowid(id)).await
    }

    /// 按 key 局部更新;等价于同步 [`Namespace::update`](crate::Namespace::update)。
    ///
    /// # Errors
    /// 库已关闭、补丁字段限额/有限值校验失败、目标不可见等(同同步 API)。
    pub async fn update(&self, key: &str, patch: UpdatePatch) -> Result<UpdateOutcome> {
        let inner = self.inner.clone();
        let key = key.to_string();
        run_blocking(move || inner.update(&key, patch)).await
    }

    /// 按 `RowId` 局部更新;等价于同步 [`Namespace::update_by_rowid`](crate::Namespace::update_by_rowid)。
    ///
    /// # Errors
    /// 库已关闭、补丁字段限额/有限值校验失败、目标不可见等(同同步 API)。
    pub async fn update_by_rowid(&self, id: RowId, patch: UpdatePatch) -> Result<UpdateOutcome> {
        let inner = self.inner.clone();
        run_blocking(move || inner.update_by_rowid(id, patch)).await
    }

    /// 信念修订;等价于同步 [`Namespace::supersede`](crate::Namespace::supersede)。
    ///
    /// # Errors
    /// 库已关闭、记录字段限额/有限值校验失败、目标不可见或 key 冲突(同同步 API)。
    pub async fn supersede(&self, key: &str, rec: Record) -> Result<UpdateOutcome> {
        let inner = self.inner.clone();
        let key = key.to_string();
        run_blocking(move || inner.supersede(&key, rec)).await
    }

    /// 按 key 记一次访问;等价于同步 [`Namespace::touch`](crate::Namespace::touch)。
    ///
    /// # Errors
    /// 库已关闭;`boost` 含非有限值时返回 `NonFinite`(同同步 API)。
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
    ///
    /// # Errors
    /// 库已关闭;`boost` 含非有限值时返回 `NonFinite`(同同步 API)。
    pub async fn touch_by_rowid(&self, id: RowId, boost: Option<f32>) -> Result<bool> {
        let inner = self.inner.clone();
        run_blocking(move || inner.touch_by_rowid(id, boost)).await
    }

    /// 记录用户反馈;等价于同步 [`Namespace::feedback`](crate::Namespace::feedback)。
    ///
    /// # Errors
    /// 库已关闭;`feedback`/`query_id` 校验同同步 API。
    ///
    /// # Returns
    /// 首次生效返回 `true`;记录不可见或同 `(RowId, QueryId)` 幂等键重复返回 `false`。
    pub async fn feedback(&self, id: RowId, feedback: Feedback, query_id: QueryId) -> Result<bool> {
        let inner = self.inner.clone();
        run_blocking(move || inner.feedback(id, feedback, query_id)).await
    }

    /// 按过滤条件遗忘;等价于同步 [`Namespace::forget`](crate::Namespace::forget)。
    ///
    /// # Errors
    /// 库已关闭;过滤表达式校验同同步 API。
    ///
    /// # Returns
    /// 被遗忘的记录数。
    pub async fn forget(&self, filter: Expr) -> Result<usize> {
        let inner = self.inner.clone();
        run_blocking(move || inner.forget(filter)).await
    }

    /// 按遗忘策略回收;等价于同步 [`Namespace::retain`](crate::Namespace::retain)。
    ///
    /// # Errors
    /// 库已关闭;`policy` 参数非法时返回 `Config`(同同步 API)。
    pub async fn retain(&self, policy: Retention) -> Result<RetainReport> {
        let inner = self.inner.clone();
        run_blocking(move || inner.retain(policy)).await
    }

    /// 记忆沉淀;等价于同步 [`Namespace::consolidate`](crate::Namespace::consolidate)。
    ///
    /// # Errors
    /// 库已关闭;`policy` 参数非法时返回 `Config`(同同步 API)。
    pub async fn consolidate(&self, policy: ConsolidationPolicy) -> Result<ConsolidateReport> {
        let inner = self.inner.clone();
        run_blocking(move || inner.consolidate(policy)).await
    }
}
