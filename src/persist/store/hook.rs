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
        let mut wal = self
            .wal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let batch = ops.len() > 1;
        if batch {
            wal.append(
                0,
                FrameKind::BatchBegin,
                &wal::encode_batch_begin(ops.len() as u32),
            )?;
        }
        for op in ops {
            append_op(&mut wal, op)?;
        }
        if batch {
            let crc = crc32(&[ops.len() as u8]);
            wal.append(
                0,
                FrameKind::BatchCommit,
                &wal::encode_batch_commit(ops.len() as u32, crc),
            )?;
        }
        // 整个写事务只 fsync 一次(组提交)。
        wal.sync()?;
        Ok(())
    }

    fn maybe_flush(&self, ws: &WriterState, config: &Config) -> Result<()> {
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

/// 追加单条写操作到 WAL(不含批边界与 fsync)。
fn append_op(wal: &mut WalWriter, op: &WriteOp) -> Result<()> {
    let (seqno, kind) = op_dispatch(op);
    let payload = op_payload(op)?;
    wal.append(seqno, kind, &payload)
}

/// 取写操作的 `(seqno, 帧类型)`。
fn op_dispatch(op: &WriteOp) -> (u64, FrameKind) {
    match op {
        WriteOp::NsRegister { .. } => (0, FrameKind::NsRegister),
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
        WriteOp::Insert { slot } => {
            let entry = entry_from_slot(slot);
            wal::encode_insert(&entry, &slot.vector)?
        }
        WriteOp::DeleteRow { rowid, .. } => wal::encode_delete_row(rowid.get()),
        WriteOp::Access {
            rowid,
            at_ms,
            importance_delta,
            ..
        } => wal::encode_touch_row(rowid.get(), *at_ms, 1, *importance_delta),
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
