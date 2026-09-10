//! delta 条目解码(`delta/decode.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::{self, Meta};
use crate::persist::Cursor;

use super::update_mask;
use super::{
    DeltaEntry, EntryHead, KIND_ACCESS, KIND_DELETE_KEY, KIND_DELETE_ROW, KIND_RELATE,
    KIND_UNRELATE, KIND_UPDATE_ROW, UpdateFields,
};

/// `UpdateFields::valid_time` 的编码形态。
type UpdateValidTime = Option<Option<(i64, Option<i64>)>>;

/// 解码单条 delta 条目。
pub(super) fn decode_entry(cursor: &mut Cursor<'_>) -> Result<DeltaEntry> {
    let kind = cursor.u8()?;
    let head = EntryHead {
        seqno: cursor.u64()?,
        tx_ms: cursor.i64()?,
        ns_id: cursor.u32()?,
    };
    match kind {
        KIND_DELETE_KEY => decode_delete_key(cursor, head),
        KIND_DELETE_ROW => Ok(DeltaEntry::DeleteRow {
            seqno: head.seqno,
            tx_ms: head.tx_ms,
            ns_id: head.ns_id,
            rowid: cursor.u64()?,
        }),
        KIND_UPDATE_ROW => decode_update_row(cursor, head),
        KIND_ACCESS => Ok(DeltaEntry::Access {
            seqno: head.seqno,
            tx_ms: head.tx_ms,
            ns_id: head.ns_id,
            rowid: cursor.u64()?,
            last_access_ms: cursor.i64()?,
            access_delta: cursor.u32()?,
            importance_delta: read_f32(cursor)?,
        }),
        KIND_RELATE => decode_relate(cursor, head),
        KIND_UNRELATE => Ok(DeltaEntry::Unrelate {
            seqno: head.seqno,
            tx_ms: head.tx_ms,
            ns_id: head.ns_id,
            from: cursor.u64()?,
            to: cursor.u64()?,
            kind: cursor.u16()?,
        }),
        other => Err(MnemeError::Corrupted {
            segment: None,
            reason: format!("delta: 未知条目 kind={other}"),
        }),
    }
}

/// 解码 `DeleteKey` 条目体。
fn decode_delete_key(cursor: &mut Cursor<'_>, head: EntryHead) -> Result<DeltaEntry> {
    let len = cursor.u32()? as usize;
    let key = std::str::from_utf8(cursor.take(len)?).map_err(|error| MnemeError::Corrupted {
        segment: None,
        reason: format!("delta: key 非 UTF-8:{error}"),
    })?;
    Ok(DeltaEntry::DeleteKey {
        seqno: head.seqno,
        tx_ms: head.tx_ms,
        ns_id: head.ns_id,
        key: Arc::from(key),
    })
}

/// 解码 `UpdateRow` 条目体。
fn decode_update_row(cursor: &mut Cursor<'_>, head: EntryHead) -> Result<DeltaEntry> {
    let rowid = cursor.u64()?;
    let mask = cursor.u8()?;
    let fields = decode_update_fields(cursor, mask)?;
    Ok(DeltaEntry::UpdateRow {
        seqno: head.seqno,
        tx_ms: head.tx_ms,
        ns_id: head.ns_id,
        rowid,
        fields,
    })
}

/// 解码 `Relate` 条目体。
fn decode_relate(cursor: &mut Cursor<'_>, head: EntryHead) -> Result<DeltaEntry> {
    let from = cursor.u64()?;
    let to = cursor.u64()?;
    let kind = cursor.u16()?;
    let weight = read_f32(cursor)?;
    let meta_len = cursor.u32()? as usize;
    let meta = meta::from_bytes(cursor.take(meta_len)?)?;
    Ok(DeltaEntry::Relate {
        seqno: head.seqno,
        tx_ms: head.tx_ms,
        ns_id: head.ns_id,
        from,
        to,
        kind,
        weight,
        meta,
    })
}

/// 按掩码解码 `UpdateFields`。
fn decode_update_fields(cursor: &mut Cursor<'_>, mask: u8) -> Result<UpdateFields> {
    use update_mask::*;
    let mut fields = UpdateFields::default();
    if mask & TEXT != 0 {
        fields.text = decode_update_text(cursor)?;
    }
    if mask & META != 0 {
        fields.meta = decode_update_meta(cursor)?;
    }
    if mask & TTL != 0 {
        fields.expires_at_ms = decode_update_i64(cursor)?;
    }
    if mask & IMPORTANCE != 0 {
        fields.importance = Some(read_f32(cursor)?);
    }
    if mask & CONFIDENCE != 0 {
        fields.confidence = Some(read_f32(cursor)?);
    }
    if mask & VALID_TIME != 0 {
        fields.valid_time = decode_update_valid_time(cursor)?;
    }
    Ok(fields)
}

/// 读取小端 `f32`。
fn read_f32(cursor: &mut Cursor<'_>) -> Result<f32> {
    Ok(f32::from_le_bytes(
        cursor.take(4)?.try_into().unwrap_or([0; 4]),
    ))
}

/// 读取带 `u32` 长度前缀的 UTF-8 字段。
fn read_utf8_field(cursor: &mut Cursor<'_>, field: &str) -> Result<Arc<str>> {
    let len = cursor.u32()? as usize;
    let value = std::str::from_utf8(cursor.take(len)?).map_err(|error| MnemeError::Corrupted {
        segment: None,
        reason: format!("delta: {field} 非 UTF-8:{error}"),
    })?;
    Ok(Arc::from(value))
}

/// 解码可清除的文本覆盖。
fn decode_update_text(cursor: &mut Cursor<'_>) -> Result<Option<Option<Arc<str>>>> {
    if cursor.u8()? == 0 {
        return Ok(Some(None));
    }
    Ok(Some(Some(read_utf8_field(cursor, "text")?)))
}

/// 解码可清除的元数据覆盖。
fn decode_update_meta(cursor: &mut Cursor<'_>) -> Result<Option<Option<Meta>>> {
    if cursor.u8()? == 0 {
        return Ok(Some(None));
    }
    let len = cursor.u32()? as usize;
    Ok(Some(Some(meta::from_bytes(cursor.take(len)?)?)))
}

/// 解码可清除的 `i64` 覆盖。
fn decode_update_i64(cursor: &mut Cursor<'_>) -> Result<Option<Option<i64>>> {
    if cursor.u8()? == 0 {
        return Ok(Some(None));
    }
    Ok(Some(Some(cursor.i64()?)))
}

/// 解码可清除的有效时间覆盖。
fn decode_update_valid_time(cursor: &mut Cursor<'_>) -> Result<UpdateValidTime> {
    if cursor.u8()? == 0 {
        return Ok(Some(None));
    }
    let valid_from = cursor.i64()?;
    let has_to = cursor.u8()? != 0;
    let valid_to = if has_to { Some(cursor.i64()?) } else { None };
    Ok(Some(Some((valid_from, valid_to))))
}
