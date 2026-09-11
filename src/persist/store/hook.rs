//! [`PersistHook`] 实现:WAL-before-visible 落盘(`store/hook.rs`)。
//!
//! 内存引擎的每个写事务经此追加 WAL;WAL 达到阈值时触发全量快照 flush,
//! 保证 WAL 有界。

use crate::core::error::{MnemeError, Result};
use crate::memory::config::Config;
use crate::memory::table::{PersistHook, SlotData, WriteOp, WriterState};
use crate::persist::crc32;
use crate::persist::msec::EntryData;
use crate::persist::wal::{self, FrameKind};

use super::Store;
use super::wal_writer::WalWriter;

impl PersistHook for Store {
    fn log(&self, ops: &[WriteOp]) -> Result<()> {
        // 空事务(如 `close` 只置 closed 标志)在只读下亦无写入,直接成功;
        // 真正的写操作在只读模式返回 `Unsupported`。
        if ops.is_empty() {
            return Ok(());
        }
        if self.read_only {
            return Err(MnemeError::Unsupported {
                feature: "只读模式写入",
            });
        }
        let batch_count = u32::try_from(ops.len()).map_err(|_| MnemeError::TooLarge {
            field: "wal batch",
            limit: u32::MAX as usize,
            got: ops.len(),
        })?;
        let mut wal = self
            .wal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 事务起点:任一 append/sync 失败即截断回此处,丢弃半写/未确认帧。
        // 否则「返回 Err 却已落盘」或残帧永久遮挡后续已 fsync 的写(FC-PERSIST-INV-001)。
        let start = wal.len()?;
        if let Err(error) = append_transaction(&mut wal, ops, batch_count) {
            // reason: 回滚失败无法再传播(仅能保留原错误);尽力截断以恢复一致性。
            wal.rollback_to(start).ok();
            return Err(error);
        }
        Ok(())
    }

    fn maybe_flush(&self, ws: &mut WriterState, config: &Config) -> Result<()> {
        if self.read_only {
            return Ok(());
        }
        let limit = config.compaction.wal_bytes;
        // L2 兜底:WAL 达到上限即全量快照 flush,保证 WAL 有界(I4)。
        if limit > 0 && self.wal_bytes() >= limit {
            self.flush(ws, config)?;
        }
        Ok(())
    }
}

/// 追加一个写事务的全部帧并组提交 fsync;任一步失败即返回错误(由调用方回滚)。
fn append_transaction(wal: &mut WalWriter, ops: &[WriteOp], batch_count: u32) -> Result<()> {
    let batch = ops.len() > 1;
    if batch {
        wal.append(
            0,
            FrameKind::BatchBegin,
            &wal::encode_batch_begin(batch_count),
        )?;
    }
    // 批 CRC 覆盖批内全部帧负载;回放时按计数与 CRC 校验原子边界(设计 04 §3.3)。
    let mut batch_crc_input = Vec::new();
    for op in ops {
        let (seqno, kind) = op_dispatch(op);
        let payload = op_payload(op)?;
        if batch {
            batch_crc_input.extend_from_slice(&payload);
        }
        wal.append(seqno, kind, &payload)?;
    }
    if batch {
        let crc = crc32(&batch_crc_input);
        wal.append(
            0,
            FrameKind::BatchCommit,
            &wal::encode_batch_commit(batch_count, crc),
        )?;
    }
    // 整个写事务只 fsync 一次(组提交)。
    wal.sync()
}

/// 取写操作的 `(seqno, 帧类型)`。
fn op_dispatch(op: &WriteOp) -> (u64, FrameKind) {
    match op {
        WriteOp::NsRegister { .. } => (0, FrameKind::NsRegister),
        WriteOp::NsUnregister { .. } => (0, FrameKind::NsUnregister),
        WriteOp::Insert { slot } => (slot.seqno.get(), FrameKind::Insert),
        WriteOp::DeleteRow { seqno, .. } => (seqno.get(), FrameKind::DeleteRow),
        WriteOp::Access { seqno, .. } => (seqno.get(), FrameKind::TouchRow),
        WriteOp::Relate { seqno, .. } => (seqno.get(), FrameKind::Relate),
        WriteOp::Unrelate { seqno, .. } => (seqno.get(), FrameKind::Unrelate),
    }
}

/// 编码写操作的 WAL 负载。
fn op_payload(op: &WriteOp) -> Result<Vec<u8>> {
    Ok(match op {
        WriteOp::NsRegister { ns_id, path } => wal::encode_ns_register(*ns_id, path),
        WriteOp::NsUnregister { ns_id } => wal::encode_ns_unregister(*ns_id),
        WriteOp::Insert { slot } => {
            let entry = entry_from_slot(slot);
            wal::encode_insert(&entry, &slot.vector, slot.tx_ms)?
        }
        WriteOp::DeleteRow { rowid, tx_ms, .. } => wal::encode_delete_row(rowid.get(), *tx_ms),
        WriteOp::Access {
            rowid,
            at_ms,
            access_delta,
            importance_delta,
            ..
        } => wal::encode_touch_row(rowid.get(), *at_ms, *access_delta, *importance_delta),
        WriteOp::Relate {
            from,
            to,
            kind,
            weight,
            meta,
            ..
        } => wal::encode_relate(
            from.get(),
            to.get(),
            wal::RelateSpec {
                kind: *kind,
                weight: *weight,
                meta,
            },
        ),
        WriteOp::Unrelate { from, to, kind, .. } => {
            wal::encode_unrelate(from.get(), to.get(), *kind)
        }
    })
}

/// 由槽位构造 WAL/msec 记录体。
fn entry_from_slot(slot: &SlotData) -> EntryData {
    EntryData {
        rowid: slot.rowid,
        seqno: slot.seqno,
        ns_id: slot.ns_id,
        key: slot.key.clone(),
        text: slot.text.clone(),
        meta: slot.meta.clone(),
        created_at_ms: slot.created_at,
        expires_at_ms: slot.expires_at,
        importance: Some(slot.importance),
        access: None,
        valid_time: Some((slot.valid_from, slot.valid_to)),
        confidence: Some(slot.confidence),
        provenance: slot.provenance.clone(),
    }
}
