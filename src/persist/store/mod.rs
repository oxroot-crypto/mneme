//! L2 持久化协调句柄 `Store`(`store/mod.rs`)。
//!
//! `Store` 持有目录、独占锁、WAL 写入器与当前 MANIFEST,实现内存引擎的
//! [`PersistHook`](crate::memory::table::PersistHook):
//!
//! - 每条写操作由 `Table::write_tx` 交给 `Store::log`,先追加 WAL 再允许可见
//!   (WAL-before-visible,设计 04 §3.1);多操作批以 `BatchBegin/Commit` 包裹;
//! - `Store::flush` 把整个写状态物化为新段并提交新 MANIFEST(全量快照,L2 兜底),
//!   随后重置 WAL(Checkpoint)。
//!
//! > 该模块是对设计 04 §1 模块清单的必要补充(L2 需要一个协调句柄);已同步到
//! > 01 §4 与 04 §1。
//!
//! # 子模块
//!
//! * `handle` —— 协调句柄 [`Store`] 的定义与统计/校验辅助。
//! * `wal_writer` —— WAL 写入器(追加 / fsync / Checkpoint 重置)。
//! * `open` —— 打开或新建持久库,解析身份并重建写状态。
//! * `manifest_io` —— MANIFEST 载入 / 提交 / 裁剪与段文件清理。
//! * `snapshot` —— 全量快照 flush 与备份。
//! * `hook` —— [`PersistHook`](crate::memory::table::PersistHook) 实现(WAL 落盘)。

mod handle;
mod hook;
mod manifest_io;
mod open;
mod snapshot;
mod wal_writer;

pub(crate) use handle::{ManifestState, Store};
pub(crate) use open::OpenOptions;
