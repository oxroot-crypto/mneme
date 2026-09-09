//! 命名空间句柄 `Namespace`(`namespace.rs`)。
//!
//! 结构定义在此;写路径、读路径、生命周期与关系方法分置于子模块,均以
//! `impl Namespace` 扩展。键唯一性按命名空间隔离。

use std::sync::Arc;

use crate::memory::config::Config;
use crate::memory::table::Table;

mod access;
mod life;
mod query;
mod relation;
mod write;

pub(crate) use query::point_get;

/// `search` 的 `top_k` 缺省值。
pub(crate) const DEFAULT_TOP_K: usize = 10;
/// 命名空间句柄;键唯一性按命名空间隔离。
#[derive(Clone)]
pub struct Namespace {
    pub(crate) table: Arc<Table>,
    pub(crate) config: Arc<Config>,
    pub(crate) ns_path: Arc<str>,
}

impl std::fmt::Debug for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Namespace")
            .field("path", &self.ns_path)
            .finish_non_exhaustive()
    }
}
