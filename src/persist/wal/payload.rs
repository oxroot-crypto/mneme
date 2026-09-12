//! 各 WAL 帧类型的负载编解码(`wal/payload.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::{self, Meta};
use crate::persist::msec::{self, EntryData};
use crate::persist::{Cursor, put_bytes_u32, put_i64, put_u32, put_u64};

/// `Relate` 负载的标量字段(权重与元数据)。
#[derive(Debug, Clone, Copy)]
pub(crate) struct RelateSpec<'a> {
    /// 关系类型编号。
    pub(crate) kind: u16,
    /// 关系权重。
    pub(crate) weight: f32,
    /// 关系元数据。
    pub(crate) meta: &'a Meta,
}

/// 编码 `Insert` 负载:`[记录体(含长度前缀)][i64 tx_ms][u32 dim][f32 × dim]`。
///
/// 记录体(设计 04 §2.3)只含元数据不含向量,故 WAL 追加向量副本以支持崩溃恢复;
/// 该字段是对设计 §2.3 的必要补全(否则未 flush 的记录重启后向量丢失)。
/// `tx_ms` 是版本事务时间,独立于记录体的 `created_at_ms`(后者在 update/touch 后
/// 保持不变),缺失会使崩溃恢复后的 `as_of` 历史错乱。
///
/// # Errors
/// 记录体或向量长度超限时返回 [`MnemeError::TooLarge`]。
pub(crate) fn encode_insert(entry: &EntryData, vector: &[f32], tx_ms: i64) -> Result<Vec<u8>> {
    let mut out = msec::encode_entry(entry)?;
    put_i64(&mut out, tx_ms);
    let dim = u32::try_from(vector.len()).map_err(|_| MnemeError::TooLarge {
        field: "wal insert vector",
        limit: u32::MAX as usize,
        got: vector.len(),
    })?;
    put_u32(&mut out, dim);
    for value in vector {
        out.extend_from_slice(&value.to_le_bytes());
    }
    Ok(out)
}

/// 解码 `Insert` 负载,返回 `(记录体, 向量, tx_ms)`。
///
/// # Errors
/// 记录体或向量损坏时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_insert(payload: &[u8]) -> Result<(EntryData, Vec<f32>, i64)> {
    let entry = msec::entry_from_prefix(payload)?;
    let total_len = u32::from_le_bytes(payload[0..4].try_into().unwrap_or([0; 4])) as usize;
    let vector_start = 4 + total_len;
    let mut cursor = Cursor::new(&payload[vector_start..], "wal insert 向量");
    let tx_ms = cursor.i64()?;
    let dim = cursor.u32()? as usize;
    // 按剩余字节数上界预分配,避免损坏文件里的大 dim 触发超额分配(设计 04 §11)。
    let mut vector = Vec::with_capacity(dim.min(cursor.remaining() / 4));
    for _ in 0..dim {
        vector.push(f32::from_le_bytes(
            cursor.take(4)?.try_into().unwrap_or([0; 4]),
        ));
    }
    Ok((entry, vector, tx_ms))
}

/// 编码 `Delete` 负载 `[NsId u32][key len+bytes]`。
///
/// 仅测试/协议夹具使用:运行时删除走 `DeleteRow`。
#[cfg(test)]
pub(crate) fn encode_delete(ns_id: u32, key: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, ns_id);
    put_bytes_u32(&mut out, key.as_bytes());
    out
}

/// 解码 `Delete` 负载。
///
/// 仅测试/协议夹具使用:运行时删除走 `DeleteRow`。
///
/// # Errors
/// 长度越界或 key 非 UTF-8 时返回 [`MnemeError::Corrupted`]。
#[cfg(test)]
pub(crate) fn decode_delete(payload: &[u8]) -> Result<(u32, Arc<str>)> {
    let mut cursor = Cursor::new(payload, "wal delete");
    let ns_id = cursor.u32()?;
    let len = cursor.u32()? as usize;
    let key = std::str::from_utf8(cursor.take(len)?).map_err(|error| MnemeError::Corrupted {
        segment: None,
        reason: format!("wal: key 非 UTF-8:{error}"),
    })?;
    Ok((ns_id, Arc::from(key)))
}

/// 编码 `DeleteRow` 负载 `[RowId][i64 tx_ms]`。
///
/// `tx_ms` 用于崩溃恢复后重建墓碑的事务时间,使删除前的 `as_of` 历史正确。
pub(crate) fn encode_delete_row(rowid: u64, tx_ms: i64) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, rowid);
    put_i64(&mut out, tx_ms);
    out
}

/// 解码 `DeleteRow` 负载,返回 `(RowId, tx_ms)`。
///
/// # Errors
/// 长度不足时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_delete_row(payload: &[u8]) -> Result<(u64, i64)> {
    let mut cursor = Cursor::new(payload, "wal delete_row");
    Ok((cursor.u64()?, cursor.i64()?))
}

/// 编码 `TouchRow` 负载 `[RowId][i64 at_ms][u32 access_delta][f32 importance_delta]`。
pub(crate) fn encode_touch_row(
    rowid: u64,
    at_ms: i64,
    access_delta: u32,
    importance_delta: f32,
) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, rowid);
    put_i64(&mut out, at_ms);
    put_u32(&mut out, access_delta);
    out.extend_from_slice(&importance_delta.to_le_bytes());
    out
}

/// 解码 `TouchRow` 负载。
///
/// # Errors
/// 长度不足时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_touch_row(payload: &[u8]) -> Result<(u64, i64, u32, f32)> {
    let mut cursor = Cursor::new(payload, "wal touch_row");
    let rowid = cursor.u64()?;
    let at_ms = cursor.i64()?;
    let access_delta = cursor.u32()?;
    let importance_delta = f32::from_le_bytes(cursor.take(4)?.try_into().unwrap_or([0; 4]));
    Ok((rowid, at_ms, access_delta, importance_delta))
}

/// 编码 `Relate` 负载 `[from][to][kind u16][f32 weight][meta len+bytes]`。
pub(crate) fn encode_relate(from: u64, to: u64, spec: RelateSpec<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, from);
    put_u64(&mut out, to);
    out.extend_from_slice(&spec.kind.to_le_bytes());
    out.extend_from_slice(&spec.weight.to_le_bytes());
    put_bytes_u32(&mut out, &meta::to_bytes(spec.meta));
    out
}

/// 解码 `Relate` 负载。
///
/// # Errors
/// 长度越界或 meta 损坏时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_relate(payload: &[u8]) -> Result<(u64, u64, u16, f32, Meta)> {
    let mut cursor = Cursor::new(payload, "wal relate");
    let from = cursor.u64()?;
    let to = cursor.u64()?;
    let kind = cursor.u16()?;
    let weight = f32::from_le_bytes(cursor.take(4)?.try_into().unwrap_or([0; 4]));
    let meta_len = cursor.u32()? as usize;
    let meta = meta::from_bytes(cursor.take(meta_len)?)?;
    Ok((from, to, kind, weight, meta))
}

/// 编码 `Unrelate` 负载 `[from][to][kind u16]`。
pub(crate) fn encode_unrelate(from: u64, to: u64, kind: u16) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, from);
    put_u64(&mut out, to);
    out.extend_from_slice(&kind.to_le_bytes());
    out
}

/// 解码 `Unrelate` 负载。
///
/// # Errors
/// 长度不足时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_unrelate(payload: &[u8]) -> Result<(u64, u64, u16)> {
    let mut cursor = Cursor::new(payload, "wal unrelate");
    Ok((cursor.u64()?, cursor.u64()?, cursor.u16()?))
}

/// 编码 `Checkpoint` 负载。
///
/// 仅测试/协议夹具使用:L2 Checkpoint 经 MANIFEST 水位 + `WalWriter::reset` 落地。
#[cfg(test)]
pub(crate) fn encode_checkpoint(watermark_seqno: u64) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, watermark_seqno);
    out
}

/// 解码 `Checkpoint` 负载。
///
/// 仅测试/协议夹具使用。
///
/// # Errors
/// 长度不足时返回 [`MnemeError::Corrupted`]。
#[cfg(test)]
pub(crate) fn decode_checkpoint(payload: &[u8]) -> Result<u64> {
    let mut cursor = Cursor::new(payload, "wal checkpoint");
    cursor.u64()
}

/// 编码 `BatchBegin` 负载。
pub(crate) fn encode_batch_begin(count: u32) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, count);
    out
}

/// 编码 `BatchCommit` 负载 `[count][batch_crc]`。
pub(crate) fn encode_batch_commit(count: u32, batch_crc: u32) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, count);
    put_u32(&mut out, batch_crc);
    out
}

/// 解码 `BatchCommit` 负载。
///
/// # Errors
/// 长度不足时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_batch_commit(payload: &[u8]) -> Result<(u32, u32)> {
    let mut cursor = Cursor::new(payload, "wal batch_commit");
    Ok((cursor.u32()?, cursor.u32()?))
}

/// 编码 `NsRegister` 负载 `[NsId][path len+bytes]`。
pub(crate) fn encode_ns_register(ns_id: u32, path: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, ns_id);
    put_bytes_u32(&mut out, path.as_bytes());
    out
}

/// 解码 `NsRegister` 负载。
///
/// # Errors
/// 长度越界或 path 非 UTF-8 时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_ns_register(payload: &[u8]) -> Result<(u32, Arc<str>)> {
    let mut cursor = Cursor::new(payload, "wal ns_register");
    let ns_id = cursor.u32()?;
    let len = cursor.u32()? as usize;
    let path = std::str::from_utf8(cursor.take(len)?).map_err(|error| MnemeError::Corrupted {
        segment: None,
        reason: format!("wal: path 非 UTF-8:{error}"),
    })?;
    Ok((ns_id, Arc::from(path)))
}

/// 编码 `NsUnregister` 负载 `[NsId]`。
pub(crate) fn encode_ns_unregister(ns_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, ns_id);
    out
}

/// 解码 `NsUnregister` 负载。
///
/// # Errors
/// 长度不足时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_ns_unregister(payload: &[u8]) -> Result<u32> {
    Cursor::new(payload, "wal ns_unregister").u32()
}
