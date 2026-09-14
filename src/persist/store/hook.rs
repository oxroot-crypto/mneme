//! [`PersistHook`] 实现:WAL-before-visible 落盘(`store/hook.rs`)。
//!
//! 内存引擎的每个写事务经此追加 WAL;WAL 达容量上限时触发增量段 flush,
//! 保证 WAL 有界(设计 04 §3.2、07 §4)。

/// WAL 硬上限相对软阈值 `wal_bytes` 的倍数(见 `maybe_flush`)。
///
/// 取 12:默认 256MiB 软阈值下允许攒到 3GiB WAL。1536 维每行约 6.3KiB,
/// 3GiB ≈ 45 万行 ≈ 7 个 65,536 行块,8 核可基本吃满;每块编码峰值约 0.7GB,
/// 7 块并行约 5GB,加向量区(约 6GB/1M×1536)在正式门槛的 16GB 机器内。
const FLUSH_WAL_HARD_FACTOR: u64 = 12;

use crate::core::error::{MnemeError, Result};
use crate::memory::config::Config;
use crate::memory::table::{PersistHook, SlotData, WriteOp, WriterState};
use crate::persist::msec::EntryData;
use crate::persist::wal::{self, FrameKind};

use super::Store;
use super::snapshot::{flush_chunk_rows, flush_parallelism};
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
        if let Err(error) = append_transaction(&mut wal, ops, batch_count, self.compression) {
            return self.fail_log(&mut wal, start, error);
        }
        self.emit_write(ops);
        Ok(())
    }

    fn maybe_flush(&self, ws: &mut WriterState, config: &Config) -> Result<()> {
        if self.read_only {
            return Ok(());
        }
        let soft = config.compaction.wal_bytes;
        let wal = self.wal_bytes();
        // 容量兜底(WAL 有界,I4):`wal_bytes` 为软阈值,达到后优先攒够
        // 「并行度 × 块行数」的可并行行数再 flush——大维度下单次只物化一个块会
        // 退化为单核串行(1536 维 256MiB 仅约 4 万行,不足一个 65,536 行块);
        // 硬上限 `soft × FLUSH_WAL_HARD_FACTOR` 兜底,保证 WAL 仍严格有界。
        if soft == 0 || wal < soft {
            return Ok(());
        }
        let hard = soft.saturating_mul(FLUSH_WAL_HARD_FACTOR);
        let parallel_rows =
            flush_parallelism(&config.tuning).saturating_mul(flush_chunk_rows(&config.tuning));
        if wal >= hard || ws.unpersisted_count() >= parallel_rows {
            self.flush(ws, config)?;
        }
        Ok(())
    }
}

impl Store {
    /// 追加失败时回滚 WAL 到 `start` 并上报 `Error` 事件,返回原错误。
    fn fail_log(&self, wal: &mut WalWriter, start: u64, error: MnemeError) -> Result<()> {
        // reason: 回滚失败无法再传播(仅能保留原错误);尽力截断以恢复一致性。
        wal.rollback_to(start).ok();
        crate::core::observe::emit(
            self.observer.as_ref(),
            crate::core::observe::Event::Error {
                kind: crate::core::observe::ErrorKind::of(&error),
                context: "persist::log",
            },
        );
        Err(error)
    }

    /// 上报写事务提交事件(负载字节 = 实际编码的 WAL 帧负载之和)。
    ///
    /// 未注册 Observer 时直接返回:事件为尽力而为,不为此重编码整批负载。
    fn emit_write(&self, ops: &[WriteOp]) {
        if self.observer.is_none() {
            return;
        }
        let bytes: usize = ops
            .iter()
            .filter_map(|op| op_payload(op, self.compression).ok().map(|p| p.len()))
            .sum();
        if let Some(op) = observer_write_op(ops) {
            crate::core::observe::emit(
                self.observer.as_ref(),
                crate::core::observe::Event::Write {
                    op,
                    took: std::time::Duration::ZERO,
                    bytes,
                },
            );
        }
    }
}

/// 追加一个写事务的全部帧并组提交 fsync;任一步失败即返回错误(由调用方回滚)。
fn append_transaction(
    wal: &mut WalWriter,
    ops: &[WriteOp],
    batch_count: u32,
    compression: crate::core::options::Compression,
) -> Result<()> {
    let batch = ops.len() > 1;
    if batch {
        wal.append(
            0,
            FrameKind::BatchBegin,
            &wal::encode_batch_begin(batch_count),
        )?;
    }
    // 批 CRC 覆盖批内全部帧负载;回放时按计数与 CRC 校验原子边界(设计 04 §3.3)。
    // 增量吸收各帧负载,免整批负载再拼接一份。
    let mut batch_hasher = crc32fast::Hasher::new();
    for op in ops {
        let (seqno, kind) = op_dispatch(op);
        let payload = op_payload(op, compression)?;
        if batch {
            batch_hasher.update(&payload);
        }
        wal.append(seqno, kind, &payload)?;
    }
    if batch {
        let crc = batch_hasher.finalize();
        wal.append(
            0,
            FrameKind::BatchCommit,
            &wal::encode_batch_commit(batch_count, crc),
        )?;
    }
    // 整个写事务只 fsync 一次(组提交)。
    wal.sync()
}

/// 由内部写操作映射观测层的逻辑写操作(metadata 帧不产生 Write 事件)。
fn observer_write_op(ops: &[WriteOp]) -> Option<crate::core::observe::WriteOp> {
    use crate::core::observe::WriteOp as Observed;
    let inserts = ops
        .iter()
        .filter(|op| matches!(op, WriteOp::Insert { .. }))
        .count();
    if inserts > 1 {
        return Some(Observed::InsertBatch);
    }
    for op in ops {
        match op {
            WriteOp::Insert { .. } => return Some(Observed::Insert),
            WriteOp::DeleteRow { .. } => return Some(Observed::Delete),
            WriteOp::Access { .. } => return Some(Observed::Touch),
            WriteOp::Relate { .. } => return Some(Observed::Relate),
            WriteOp::Unrelate { .. } => return Some(Observed::Unrelate),
            WriteOp::NsRegister { .. }
            | WriteOp::NsUnregister { .. }
            | WriteOp::RelKindRegister { .. } => {}
        }
    }
    None
}

/// 取写操作的 `(seqno, 帧类型)`。
fn op_dispatch(op: &WriteOp) -> (u64, FrameKind) {
    match op {
        WriteOp::NsRegister { seqno, .. } => (seqno.get(), FrameKind::NsRegister),
        WriteOp::NsUnregister { seqno, .. } => (seqno.get(), FrameKind::NsUnregister),
        WriteOp::RelKindRegister { seqno, .. } => (seqno.get(), FrameKind::RelKindRegister),
        WriteOp::Insert { slot } => (slot.seqno.get(), FrameKind::Insert),
        WriteOp::DeleteRow { seqno, .. } => (seqno.get(), FrameKind::DeleteRow),
        WriteOp::Access { seqno, .. } => (seqno.get(), FrameKind::TouchRow),
        WriteOp::Relate { seqno, .. } => (seqno.get(), FrameKind::Relate),
        WriteOp::Unrelate { seqno, .. } => (seqno.get(), FrameKind::Unrelate),
    }
}

/// 编码写操作的 WAL 负载。
fn op_payload(op: &WriteOp, compression: crate::core::options::Compression) -> Result<Vec<u8>> {
    Ok(match op {
        WriteOp::NsRegister { ns_id, path, .. } => wal::encode_ns_register(*ns_id, path),
        WriteOp::NsUnregister { ns_id, .. } => wal::encode_ns_unregister(*ns_id),
        WriteOp::RelKindRegister { kind, name, .. } => wal::encode_rel_kind_register(*kind, name),
        WriteOp::Insert { slot } => {
            let entry = entry_from_slot(slot);
            wal::encode_insert(&entry, &slot.vector, slot.tx_ms, compression)?
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
