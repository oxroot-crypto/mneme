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
//!
//! # 取消语义
//!
//! 所有方法经 `spawn_blocking` 执行,阻塞任务一经派发不可取消:drop 返回的
//! future 只丢弃等待结果,后台同步操作仍会执行完成(写入照常生效)。需要
//! 「取消即中止」的调用方应在业务层以句柄/标志做协作取消。

mod facade;
mod read;
mod relation;
mod write;

#[cfg(feature = "async")]
pub use self::facade::AsyncNamespace;

#[cfg(test)]
use crate::core::options::{RelationKind, UpdatePatch};
#[cfg(test)]
use crate::core::types::RowId;
#[cfg(test)]
use crate::memory::record::{InsertOutcome, Record, UpdateOutcome};
#[cfg(test)]
use crate::memory::relation::RelateOptions;

#[cfg(test)]
mod tests;
