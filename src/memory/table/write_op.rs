//! 待持久化写操作与持久层钩子接口(`table/write_op.rs`)。
//!
//! [`WriteOp`] 由 [`WriterState`] 在变更点收集、`Table::write_tx` 在事务成功后
//! 统一交给持久层,保证 WAL 先于可见性写入(设计 04 §3.1),且失败时随事务回滚
//! 一并丢弃。[`PersistHook`] 由 L2 持久引擎实现,内存引擎在无钩子时行为不变。

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::meta::Meta;
use crate::core::types::{RowId, SeqNo};

use super::state::{SlotData, WriterState};

/// 一个待持久化到 WAL 的写操作(L2 由 [`PersistHook`] 消费)。
#[derive(Debug, Clone)]
pub(crate) enum WriteOp {
    /// 命名空间首次登记。
    NsRegister {
        /// 命名空间编号。
        ns_id: u32,
        /// 命名空间路径。
        path: Arc<str>,
        /// 版本序号(metadata 帧也参与水位判定,防止残留旧 WAL 复活注册表)。
        seqno: SeqNo,
    },
    /// 命名空间注销(路径删除后其 `NsId` 作废、永不复用)。
    NsUnregister {
        /// 被注销的命名空间编号。
        ns_id: u32,
        /// 版本序号(metadata 帧也参与水位判定)。
        seqno: SeqNo,
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
        /// 事务时间(Unix 毫秒);崩溃恢复重建墓碑时保持 `as_of` 历史正确。
        tx_ms: i64,
    },
    /// 访问统计更新(touch;`importance` 变更已随版本 `Insert` 记录)。
    Access {
        /// 目标 `RowId`。
        rowid: RowId,
        /// 版本序号。
        seqno: SeqNo,
        /// 访问时刻(Unix 毫秒)。
        at_ms: i64,
        /// 本次合并的访问次数增量(读路径攒批 ≥1;显式 `touch` 恒为 1)。
        access_delta: u32,
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
        _ws: &mut WriterState,
        _config: &crate::memory::config::Config,
    ) -> Result<()> {
        Ok(())
    }
}
