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
//! * `state` —— 物理槽位 [`SlotData`] 与写状态 [`WriterState`]。
//! * `view` —— 不可变读视图 [`ReaderView`]。
//! * `handle` —— 表句柄 [`Table`](含写事务与发布)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::types::{RowId, SeqNo, SlotId};

mod handle;
mod state;
mod view;

pub(crate) use handle::Table;
pub(crate) use state::{SlotData, WriterState};
pub(crate) use view::ReaderView;

/// 一个待持久化到 WAL 的写操作(L2 由 [`PersistHook`] 消费)。
///
/// 由 [`WriterState`] 在变更点收集、`write_tx` 在事务成功后统一交给持久层,
/// 保证 WAL 先于可见性写入(设计 04 §3.1),且失败时随事务回滚一并丢弃。
#[derive(Debug, Clone)]
pub(crate) enum WriteOp {
    /// 命名空间首次登记。
    NsRegister {
        /// 命名空间编号。
        ns_id: u32,
        /// 命名空间路径。
        path: Arc<str>,
    },
    /// 提交一个新物理版本(记录体)。
    Insert {
        /// 新版本的槽位数据(`deleted == false`)。
        slot: Arc<SlotData>,
    },
    /// 提交一个墓碑版本(无记录体)。
    DeleteRow {
        /// 目标 `RowId`。
        rowid: RowId,
        /// 版本序号。
        seqno: SeqNo,
    },
    /// 访问统计更新(touch;`importance` 变更已随版本 `Insert` 记录)。
    Access {
        /// 目标 `RowId`。
        rowid: RowId,
        /// 版本序号。
        seqno: SeqNo,
        /// 访问时刻(Unix 毫秒)。
        at_ms: i64,
        /// 重要度增量。
        importance_delta: f32,
    },
    /// 建立/更新关系边。
    Relate {
        /// 起点。
        from: RowId,
        /// 终点。
        to: RowId,
        /// 关系类型。
        kind: u16,
        /// 边权。
        weight: f32,
        /// 边元数据。
        meta: Meta,
        /// 版本序号。
        seqno: SeqNo,
    },
    /// 删除关系边。
    Unrelate {
        /// 起点。
        from: RowId,
        /// 终点。
        to: RowId,
        /// 关系类型。
        kind: u16,
        /// 版本序号。
        seqno: SeqNo,
    },
}

/// 持久层写日志钩子:由 L2 的持久引擎实现,内存引擎在无钩子时行为不变。
///
/// `log` 对同一写事务的多个操作按顺序原子落盘;返回 `Err` 时调用方回滚该事务,
/// 保证"WAL 失败则写入不可见"(设计 04 §3.1)。
pub(crate) trait PersistHook: Send + Sync {
    /// 顺序记录一批写操作。
    ///
    /// # Errors
    /// WAL 写入/fsync 失败时返回对应错误。
    fn log(&self, ops: &[WriteOp]) -> Result<()>;

    /// 持久层内部阈值(如 WAL 容量)越界时触发一次落盘;默认无操作。
    ///
    /// # Errors
    /// 落盘 I/O 失败时返回对应错误。
    fn maybe_flush(
        &self,
        _ws: &WriterState,
        _config: &crate::memory::config::Config,
    ) -> Result<()> {
        Ok(())
    }
}

/// 把槽位下标映射为 `SlotId`;超出 `u32::MAX` 时返回结构化错误,绝不静默饱和
/// (FC-MEM-INV-004)。
pub(crate) fn slot_id_for(len: usize) -> Result<SlotId> {
    u32::try_from(len)
        .map(SlotId::new)
        .map_err(|_| MnemeError::LimitExceeded {
            field: "slots",
            limit: u32::MAX as usize,
            got: len,
        })
}

/// 单条记录的访问统计(内存累积;L5 起落盘)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccessStat {
    /// 最近一次访问时刻(Unix 毫秒)。
    pub last_access_ms: i64,
    /// 累计访问次数。
    pub access_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FC-MEM-INV-004
    #[test]
    fn slot_id_for_rejects_overflow() {
        assert_eq!(slot_id_for(0).expect("0 合法").get(), 0);
        assert_eq!(
            slot_id_for(u32::MAX as usize).expect("上界合法").get(),
            u32::MAX
        );
        let overflow = u32::MAX as usize + 1;
        assert!(matches!(
            slot_id_for(overflow),
            Err(MnemeError::LimitExceeded {
                field: "slots",
                limit,
                got,
            }) if limit == u32::MAX as usize && got == overflow
        ));
    }
}
