//! msec 记录体解码(`msec/entry.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::{self, Meta};
use crate::core::types::{Key, NsId, RowId, SeqNo};
use crate::persist::Cursor;

use super::{
    EntryData, FLAG_ACCESS, FLAG_CONFIDENCE, FLAG_IMPORTANCE, FLAG_KEY, FLAG_PROVENANCE, FLAG_TEXT,
    FLAG_TTL, FLAG_VALID_TIME, FLAG2_META_COMPRESSED, FLAG2_PROVENANCE_COMPRESSED,
    FLAG2_TEXT_COMPRESSED,
};

/// 解码单个记录体(不含长度前缀的 `body` 字节)。
///
/// # Errors
/// 字段越界、JSON 损坏或标志位矛盾时返回 [`MnemeError::Corrupted`]。
pub(super) fn decode_entry(body: &[u8]) -> Result<EntryData> {
    let mut cursor = Cursor::new(body, "msec entry");
    let rowid = RowId::new(cursor.u64()?);
    let seqno = SeqNo::new(cursor.u64()?);
    let ns_id = NsId::new(cursor.u32()?);
    let flags = cursor.u8()?;
    let flags2 = cursor.u8()?;
    if flags2 & !(FLAG2_TEXT_COMPRESSED | FLAG2_META_COMPRESSED | FLAG2_PROVENANCE_COMPRESSED) != 0
    {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "msec: flags2 含未知位".to_string(),
        });
    }
    let key = decode_key(&mut cursor, flags)?;
    let text = decode_text(&mut cursor, flags, flags2)?;
    let meta = decode_meta(&mut cursor, flags2)?;
    let created_at_ms = cursor.i64()?;
    let expires_at_ms = if flags & FLAG_TTL != 0 {
        Some(cursor.i64()?)
    } else {
        None
    };
    let importance = decode_flag_f32(&mut cursor, flags, FLAG_IMPORTANCE)?;
    let access = decode_access(&mut cursor, flags)?;
    let valid_time = decode_valid_time(&mut cursor, flags)?;
    let confidence = decode_flag_f32(&mut cursor, flags, FLAG_CONFIDENCE)?;
    let provenance = decode_provenance(&mut cursor, flags, flags2)?;
    if !cursor.is_empty() {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "msec: 记录体尾部有残留字节".to_string(),
        });
    }
    Ok(EntryData {
        rowid,
        seqno,
        ns_id,
        key,
        text,
        meta,
        created_at_ms,
        expires_at_ms,
        importance,
        access,
        valid_time,
        confidence,
        provenance,
    })
}

/// 从带 `u32 total_len` 前缀的记录体字节解析记录(WAL `Insert` 帧用)。
///
/// # Errors
/// 长度前缀越界或记录体损坏时返回 [`MnemeError::Corrupted`]。
pub(crate) fn entry_from_prefix(bytes: &[u8]) -> Result<EntryData> {
    let mut cursor = Cursor::new(bytes, "msec entry 前缀");
    let total_len = cursor.u32()? as usize;
    decode_entry(cursor.take(total_len)?)
}

/// 读取小端 `f32`。
fn read_f32(cursor: &mut Cursor<'_>) -> Result<f32> {
    let bytes = cursor.take(4)?;
    Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// 按 `flag` 存在位读取可选 `f32`。
fn decode_flag_f32(cursor: &mut Cursor<'_>, flags: u8, flag: u8) -> Result<Option<f32>> {
    if flags & flag == 0 {
        return Ok(None);
    }
    Ok(Some(read_f32(cursor)?))
}

/// 读取带 `u32` 长度前缀的 UTF-8 字符串。
fn read_utf8(cursor: &mut Cursor<'_>, field: &str) -> Result<Arc<str>> {
    let len = cursor.u32()? as usize;
    let value = std::str::from_utf8(cursor.take(len)?).map_err(|error| MnemeError::Corrupted {
        segment: None,
        reason: format!("msec: {field} 非 UTF-8:{error}"),
    })?;
    Ok(Arc::from(value))
}

/// 按标志位解码可选 key(复用 `read_utf8` 的 `Arc`,免二次分配)。
fn decode_key(cursor: &mut Cursor<'_>, flags: u8) -> Result<Option<Key>> {
    if flags & FLAG_KEY == 0 {
        return Ok(None);
    }
    let text = read_utf8(cursor, "key")?;
    Ok(Some(Key::from_arc(text)))
}

/// 按标志位解码可选 text(压缩字段先解压再校验 UTF-8)。
fn decode_text(cursor: &mut Cursor<'_>, flags: u8, flags2: u8) -> Result<Option<Arc<str>>> {
    if flags & FLAG_TEXT == 0 {
        return Ok(None);
    }
    if flags2 & FLAG2_TEXT_COMPRESSED != 0 {
        let len = cursor.u32()? as usize;
        let raw = crate::compress::decode_field(cursor.take(len)?)?;
        let value = std::str::from_utf8(&raw).map_err(|error| MnemeError::Corrupted {
            segment: None,
            reason: format!("msec: text 非 UTF-8:{error}"),
        })?;
        return Ok(Some(Arc::from(value)));
    }
    Ok(Some(read_utf8(cursor, "text")?))
}

/// 解码 meta(压缩字段先解压)。
fn decode_meta(cursor: &mut Cursor<'_>, flags2: u8) -> Result<Meta> {
    let len = cursor.u32()? as usize;
    if flags2 & FLAG2_META_COMPRESSED != 0 {
        let raw = crate::compress::decode_field(cursor.take(len)?)?;
        return meta::from_bytes(&raw);
    }
    meta::from_bytes(cursor.take(len)?)
}

/// 按标志位解码访问统计 `(last_access_ms, access_count)`。
fn decode_access(cursor: &mut Cursor<'_>, flags: u8) -> Result<Option<(i64, u32)>> {
    if flags & FLAG_ACCESS == 0 {
        return Ok(None);
    }
    let last_access = cursor.i64()?;
    let count = cursor.u32()?;
    Ok(Some((last_access, count)))
}

/// 按标志位解码有效时间 `(valid_from, valid_to)`。
fn decode_valid_time(cursor: &mut Cursor<'_>, flags: u8) -> Result<Option<(i64, Option<i64>)>> {
    if flags & FLAG_VALID_TIME == 0 {
        return Ok(None);
    }
    let valid_from = cursor.i64()?;
    let has_to = match cursor.u8()? {
        0 => false,
        1 => true,
        _ => {
            return Err(crate::core::error::MnemeError::Corrupted {
                segment: None,
                reason: "entry: valid_time 标志非法".to_string(),
            });
        }
    };
    let valid_to = if has_to { Some(cursor.i64()?) } else { None };
    Ok(Some((valid_from, valid_to)))
}

/// 按标志位解码 provenance(压缩字段先解压)。
fn decode_provenance(cursor: &mut Cursor<'_>, flags: u8, flags2: u8) -> Result<Option<Meta>> {
    if flags & FLAG_PROVENANCE == 0 {
        return Ok(None);
    }
    let len = cursor.u32()? as usize;
    if flags2 & FLAG2_PROVENANCE_COMPRESSED != 0 {
        let raw = crate::compress::decode_field(cursor.take(len)?)?;
        return Ok(Some(meta::from_bytes(&raw)?));
    }
    Ok(Some(meta::from_bytes(cursor.take(len)?)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::msec::encode_entry;

    /// FC-PERSIST-ERR-010:记录体 `valid_time` 的 `has_to` 标志仅接受 0/1,
    /// 其它值按畸形拒绝,绝不静默当 `true`。
    #[test]
    fn invalid_valid_time_flag_is_rejected() {
        let entry = EntryData {
            rowid: RowId::new(1),
            seqno: SeqNo::new(1),
            ns_id: NsId::new(1),
            key: None,
            text: None,
            meta: Meta::Null,
            created_at_ms: 0,
            expires_at_ms: None,
            importance: None,
            access: None,
            // 用独特值定位 `valid_from` 后的 `has_to` 字节。
            valid_time: Some((i64::MIN, Some(i64::MIN + 1))),
            confidence: None,
            provenance: None,
        };
        let encoded =
            encode_entry(&entry, crate::core::options::Compression::None).expect("encode");
        let mut body = encoded[4..].to_vec();
        let pattern = i64::MIN.to_le_bytes();
        let pos = body
            .windows(8)
            .position(|window| window == pattern)
            .expect("valid_from")
            + 8;
        assert_eq!(body[pos], 1, "has_to 应编码为 1");
        body[pos] = 2;
        assert!(matches!(
            decode_entry(&body),
            Err(MnemeError::Corrupted { .. })
        ));
    }
}
