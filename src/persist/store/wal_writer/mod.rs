//! WAL 写入器与 WAL 文件集(`store/wal_writer/`)。
//!
//! 持有当前活动 WAL **相对路径**,经 [`Storage`] 后端追加/同步/轮转;按
//! [`FsyncPolicy`] 决定落盘时机,单文件达 `wal_file_bytes` 后在下一次事务提交时
//! 轮转到下一个文件(**单批不跨文件**,设计 04 §3.2)。只读实例不写入
//! (`writable = false`);任何写方法返回 `Unsupported`,且打开时绝不创建或改写
//! WAL 文件(设计 12 §2 只读共享)。
//!
//! # 子模块
//!
//! * `paths` —— WAL 文件命名、列举与序号解析。
//! * `config` —— WAL 打开参数 [`WalConfig`]。
//! * `inputs` —— 写入器打开/组装输入参数(`FromPartsInput`/`OpenExistingInput`)。
//! * `lifecycle` —— 打开/新建/只读装配与截断重建(`create_truncating`)。
//! * `writer` —— [`WalWriter`] 定义与追加/同步/轮转/Checkpoint 重置。
//!
//! [`Storage`]: crate::persist::storage::Storage
//! [`FsyncPolicy`]: crate::core::options::FsyncPolicy

mod config;
mod inputs;
mod lifecycle;
mod paths;
mod writer;

// 拆分唔改外部可达性:旧路径 `crate::persist::store::wal_writer::{...}` 照旧。
pub(super) use config::WalConfig;
pub(super) use paths::wal_files;
pub(super) use writer::WalWriter;
// `wal_name`/`wal_index_of` 仅模块内部使用,但拆分不改变原 `pub(super)` 路径可达性。
#[allow(unused_imports)]
pub(super) use paths::{wal_index_of, wal_name};

#[cfg(test)]
mod tests;
