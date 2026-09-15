//! `AsyncNamespace` 个关系边方法。

use crate::core::error::Result;
use crate::core::options::RelationKind;
use crate::core::types::RowId;
use crate::memory::relation::{Edge, RelateOptions};

use super::facade::{AsyncNamespace, run_blocking};

impl AsyncNamespace {
    /// 建立带权关系边;等价于同步 [`Namespace::relate`](crate::Namespace::relate)。
    ///
    /// # Errors
    /// 库已关闭、边权含非有限值或元数据超限(同同步 API)。
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
    ///
    /// # Errors
    /// 库已关闭、边权含非有限值或元数据超限(同同步 API)。
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
    /// # Errors
    /// 库已关闭;参数校验同同步 API。
    ///
    /// # Returns
    /// 边原先存在返回 `true`;否则 `false`。
    pub async fn unrelate(&self, from: RowId, to: RowId, kind: RelationKind) -> Result<bool> {
        let inner = self.inner.clone();
        run_blocking(move || inner.unrelate(from, to, kind)).await
    }

    /// 列出出边;等价于同步 [`Namespace::neighbors`](crate::Namespace::neighbors)。
    ///
    /// # Errors
    /// 库已关闭;参数校验同同步 API。
    pub async fn neighbors(&self, from: RowId, kinds: &[RelationKind]) -> Result<Vec<Edge>> {
        let inner = self.inner.clone();
        let kinds = kinds.to_vec();
        run_blocking(move || inner.neighbors(from, &kinds)).await
    }

    /// 列出入边;等价于同步 [`Namespace::predecessors`](crate::Namespace::predecessors)。
    ///
    /// # Errors
    /// 库已关闭;参数校验同同步 API。
    pub async fn predecessors(&self, to: RowId, kinds: &[RelationKind]) -> Result<Vec<Edge>> {
        let inner = self.inner.clone();
        let kinds = kinds.to_vec();
        run_blocking(move || inner.predecessors(to, &kinds)).await
    }
}
