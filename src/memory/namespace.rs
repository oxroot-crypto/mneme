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
mod scan;
mod write;

pub(crate) use query::point_get;

/// `search` 的 `top_k` 缺省值。
pub(crate) const DEFAULT_TOP_K: usize = 10;

/// 规范化命名空间路径:去除首尾 `/`、合并连续 `/`;空串表示根命名空间。
///
/// `namespace()` 不返回 `Result`,故此处只做无损规范化;深度/非法字符在首次
/// 写入时以 `Config` 报告(FC-LIFE-POST-005)。
pub(crate) fn normalize_path(path: &str) -> String {
    let mut normalized = String::new();
    for segment in path.split('/') {
        if segment.is_empty() {
            continue;
        }
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(segment);
    }
    normalized
}

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
