//! `AsyncNamespace` 个点读、批量读同计数方法。

use crate::core::error::Result;
use crate::core::types::RowId;
use crate::memory::pred::Expr;
use crate::memory::record::StoredRecord;

use super::facade::{AsyncNamespace, run_blocking};

impl AsyncNamespace {
    /// 按 key 点读;返回 owned [`StoredRecord`](crate::StoredRecord)
    ///
    /// # Errors
    /// 库已关闭;`key` 校验同同步 API。
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
    ///
    /// # Errors
    /// 库已关闭;`RowId` 校验同同步 API。
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
    ///
    /// # Errors
    /// 库已关闭;`keys` 校验同同步 API。
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
    ///
    /// # Errors
    /// 库已关闭;`RowId` 列表校验同同步 API。
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
    ///
    /// # Errors
    /// 库已关闭;`key` 校验同同步 API。
    pub async fn exists(&self, key: &str) -> Result<bool> {
        let inner = self.inner.clone();
        let key = key.to_string();
        run_blocking(move || inner.exists(&key)).await
    }

    /// 取向量副本;等价于同步 [`Namespace::get_vector`](crate::Namespace::get_vector)。
    ///
    /// # Errors
    /// 库已关闭;`RowId` 校验同同步 API。
    ///
    /// # Returns
    /// 记录存在时返回 `Some(f32 向量)`;否则 `None`。
    pub async fn get_vector(&self, id: RowId) -> Result<Option<Vec<f32>>> {
        let inner = self.inner.clone();
        run_blocking(move || inner.get_vector(id)).await
    }

    /// 计数(可选过滤);等价于同步 [`Namespace::count`](crate::Namespace::count)。
    ///
    /// # Errors
    /// 库已关闭;过滤表达式类型校验同同步 API。
    pub async fn count(&self, filter: Option<Expr>) -> Result<u64> {
        let inner = self.inner.clone();
        run_blocking(move || inner.count(filter)).await
    }
}
