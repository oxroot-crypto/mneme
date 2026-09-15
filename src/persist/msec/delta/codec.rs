//! delta 区编解码:区头、条目公共前缀同按 kind 个载荷。

use crate::core::error::{MnemeError, Result};
use crate::core::meta;
use crate::persist::{
    Cursor, check_version, crc32, put_bytes_u32, put_i64, put_u16, put_u32, put_u64,
};

use super::consts::{
    COMMON_BYTES, FORMAT_VERSION, HEADER_LEN, KIND_ACCESS, KIND_RELATE, KIND_UNRELATE, MAGIC,
};
use super::entry::DeltaEntry;

/// 编码 delta 区(按 `(target, seqno, kind)` 排序;空条目返回空 `Vec`)。
///
/// # Errors
/// 条目数超过 `u16::MAX` 时返回 [`MnemeError::LimitExceeded`],绝不静默截断计数
/// (截断会产出解码端判为"尾部残留"的损坏段,FC-PERSIST-ERR-011)。
pub(crate) fn encode_delta(entries: &[DeltaEntry]) -> Result<Vec<u8>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let mut sorted: Vec<&DeltaEntry> = entries.iter().collect();
    sorted.sort_by_key(|entry| (entry.target(), entry.common().1, entry.kind_rank()));
    let count = u16::try_from(sorted.len()).map_err(|_| MnemeError::LimitExceeded {
        field: "delta entries",
        limit: u16::MAX as usize,
        got: sorted.len(),
    })?;

    let mut body = Vec::new();
    for entry in &sorted {
        encode_entry(&mut body, entry);
    }

    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&MAGIC);
    put_u16(&mut out, FORMAT_VERSION);
    put_u16(&mut out, count);
    put_u32(&mut out, crc32(&body));
    out.extend_from_slice(&body);
    Ok(out)
}

/// 编码单条 delta 条目(公共前缀 + 按 kind 的载荷)。
fn encode_entry(out: &mut Vec<u8>, entry: &DeltaEntry) {
    let (kind, seqno, tx_ms, ns_id) = entry.common();
    out.push(kind);
    put_u64(out, seqno);
    put_i64(out, tx_ms);
    put_u32(out, ns_id);
    match entry {
        DeltaEntry::Access {
            rowid,
            last_access_ms,
            access_delta,
            importance_delta,
            ..
        } => {
            put_u64(out, *rowid);
            put_i64(out, *last_access_ms);
            put_u32(out, *access_delta);
            out.extend_from_slice(&importance_delta.to_le_bytes());
        }
        DeltaEntry::Relate {
            from,
            to,
            kind,
            weight,
            meta,
            ..
        } => {
            put_u64(out, *from);
            put_u64(out, *to);
            put_u16(out, *kind);
            out.extend_from_slice(&weight.to_le_bytes());
            put_bytes_u32(out, &meta::to_bytes(meta));
        }
        DeltaEntry::Unrelate { from, to, kind, .. } => {
            put_u64(out, *from);
            put_u64(out, *to);
            put_u16(out, *kind);
        }
    }
}

/// 解码 delta 区;空区返回空 `Vec`。
///
/// # Errors
/// 魔数/版本/CRC 不符、条目截断、未知 kind 或非法 meta 时返回
/// [`MnemeError::Corrupted`](FC-PERSIST-ERR-011)。
pub(crate) fn decode_delta(bytes: &[u8]) -> Result<Vec<DeltaEntry>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.len() < HEADER_LEN {
        return Err(corrupted("delta: 区短于头部"));
    }
    let mut cursor = Cursor::new(bytes, "msec delta");
    if cursor.take(4)? != MAGIC {
        return Err(corrupted("delta: 魔数不符"));
    }
    check_version("delta", cursor.u16()?, FORMAT_VERSION)?;
    let count = cursor.u16()? as usize;
    let stored_crc = cursor.u32()?;
    let body = cursor.take(cursor.remaining())?;
    if crc32(body) != stored_crc {
        return Err(corrupted("delta: 条目区 CRC 不符"));
    }
    decode_entries(body, count)
}

/// 解码条目区并拒绝尾部残留。
fn decode_entries(body: &[u8], count: usize) -> Result<Vec<DeltaEntry>> {
    let mut cursor = Cursor::new(body, "msec delta 条目");
    let mut entries = Vec::with_capacity(count.min(body.len() / COMMON_BYTES));
    for _ in 0..count {
        entries.push(decode_entry(&mut cursor)?);
    }
    if !cursor.is_empty() {
        return Err(corrupted("delta: 条目区尾部有残留字节"));
    }
    Ok(entries)
}

/// 解码单条 delta 条目。
fn decode_entry(cursor: &mut Cursor<'_>) -> Result<DeltaEntry> {
    let kind = cursor.u8()?;
    let seqno = cursor.u64()?;
    let tx_ms = cursor.i64()?;
    let ns_id = cursor.u32()?;
    Ok(match kind {
        KIND_ACCESS => DeltaEntry::Access {
            seqno,
            tx_ms,
            ns_id,
            rowid: cursor.u64()?,
            last_access_ms: cursor.i64()?,
            access_delta: cursor.u32()?,
            importance_delta: f32::from_le_bytes(cursor.take(4)?.try_into().unwrap_or([0; 4])),
        },
        KIND_RELATE => {
            let from = cursor.u64()?;
            let to = cursor.u64()?;
            let kind = cursor.u16()?;
            let weight = f32::from_le_bytes(cursor.take(4)?.try_into().unwrap_or([0; 4]));
            let meta_len = cursor.u32()? as usize;
            let meta = meta::from_bytes(cursor.take(meta_len)?)?;
            DeltaEntry::Relate {
                seqno,
                tx_ms,
                ns_id,
                from,
                to,
                kind,
                weight,
                meta,
            }
        }
        KIND_UNRELATE => DeltaEntry::Unrelate {
            seqno,
            tx_ms,
            ns_id,
            from: cursor.u64()?,
            to: cursor.u64()?,
            kind: cursor.u16()?,
        },
        _ => return Err(corrupted("delta: 未知条目种类")),
    })
}

/// 构造 delta 区损坏错误。
fn corrupted(reason: &str) -> MnemeError {
    MnemeError::Corrupted {
        segment: None,
        reason: reason.to_string(),
    }
}
