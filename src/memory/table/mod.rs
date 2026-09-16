//! 内存表结构、写状态与不可变读视图。
//!
//! 写路径由 [`Table::writer`] 串行;每次写入完成后由 [`Table::publish`] 把
//! [`WriterState`] 的 `Arc` 句柄快照成一份 [`ReaderView`] 并原子发布,
//! 读者克隆该 `Arc` 后即可无锁扫描——这是设计 03 §3/§7 的 L1 落地。
//!
//! 物理版本以 [`SlotData`] 表示,下标即 `SlotId`、**只增不减**;被遮蔽/删除的
//! 版本以 `dead` 位图标记,`as_of` 仍可经版本链读取历史。
//!
//! # 子模块
//!
//! * `state` —— 物理槽位 [`SlotData`]、写状态 [`WriterState`] 与访问统计。
//! * `write_op` —— 待持久化写操作 [`WriteOp`] 与钩子 [`PersistHook`]。
//! * `view` —— 不可变读视图 [`ReaderView`]。
//! * `handle` —— 表句柄 [`Table`](含写事务与发布)。

mod handle;
mod state;
mod view;
mod write_op;

pub(crate) use handle::Table;
pub use state::AccessStat;
pub(crate) use state::{InstallSegmentInput, SlotData, WriterState, build_segment_index};
pub(crate) use view::{CachedPlan, ReaderView};
pub(crate) use write_op::{PersistHook, WriteOp};
