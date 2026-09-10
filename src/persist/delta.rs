//! 跨段覆盖区(`delta`)编解码(设计 04 §2.2a)。
//!
//! 作用于**旧段记录**的 `delete`/`update`/`touch`/`relate` 无法原地改写不可变段,
//! 故持久化进新段的 delta 区,使 WAL 可安全截断(I19)。delta 与 msec 同 CRC、
//! 同生同灭,一经所在段提交即可参与 WAL 截断判定。
//!
//! ```text
//! 0  magic "DLT1" | 4 u16 ver | 6 u16 count | 8 u32 delta_crc32(覆盖条目区)
//! 条目 × count(按 (target, seqno) 排序):
//!   [u8 kind][u64 seqno][i64 tx_ms][u32 ns_id] 之后按 kind:
//!   1 DeleteKey : [key len+bytes]
//!   2 DeleteRow : [RowId u64]
//!   3 UpdateRow : [RowId u64][u8 field_mask][可选字段]
//!   4 Access    : [RowId u64][i64 last_access_ms][u32 access_delta][f32 importance_delta]
//!   5 Relate    : [from u64][to u64][kind u16][f32 weight][meta len+bytes]
//!   6 Unrelate  : [from u64][to u64][kind u16]
//! ```

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::{self, Meta};
use crate::persist::{
    Cursor, FORMAT_VERSION, check_version, crc32, put_bytes_u32, put_i64, put_u16, put_u32, put_u64,
};

/// delta 区魔数。
pub(crate) const MAGIC: [u8; 4] = *b"DLT1";

/// `UpdateRow` 字段掩码位。
///
/// 掩码只标记"本字段被更新";对可清除字段(text/meta/ttl/valid_time),其值前另带
/// 一个 `u8` 存在位:`0` = 清除,`1` = 后随具体值。掩码位不置位的字段保持不变。
mod update_mask {
    pub(super) const TEXT: u8 = 1 << 0;
    pub(super) const META: u8 = 1 << 1;
    pub(super) const TTL: u8 = 1 << 2;
    pub(super) const IMPORTANCE: u8 = 1 << 3;
    pub(super) const CONFIDENCE: u8 = 1 << 4;
    pub(super) const VALID_TIME: u8 = 1 << 5;
}

/// `update` 的局部字段覆盖;`None` = 不改,`Some(None)` = 清除。
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct UpdateFields {
    /// 文本覆盖。
    pub(crate) text: Option<Option<Arc<str>>>,
    /// 元数据覆盖。
    pub(crate) meta: Option<Option<Meta>>,
    /// 逻辑过期覆盖。
    pub(crate) expires_at_ms: Option<Option<i64>>,
    /// 重要度覆盖。
    pub(crate) importance: Option<f32>,
    /// 可信度覆盖。
    pub(crate) confidence: Option<f32>,
    /// 有效时间覆盖。
    pub(crate) valid_time: Option<Option<(i64, Option<i64>)>>,
}

/// 一条 delta 覆盖条目。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DeltaEntry {
    /// 按 `(ns_id, key)` 删除(遮蔽所有更早版本)。
    DeleteKey {
        /// 写序号。
        seqno: u64,
        /// 事务时间(Unix 毫秒)。
        tx_ms: i64,
        /// 命名空间编号。
        ns_id: u32,
        /// 业务 key。
        key: Arc<str>,
    },
    /// 按 `RowId` 删除。
    DeleteRow {
        /// 写序号。
        seqno: u64,
        /// 事务时间。
        tx_ms: i64,
        /// 命名空间编号。
        ns_id: u32,
        /// 目标 RowId。
        rowid: u64,
    },
    /// 按 `RowId` 局部更新。
    UpdateRow {
        /// 写序号。
        seqno: u64,
        /// 事务时间。
        tx_ms: i64,
        /// 命名空间编号。
        ns_id: u32,
        /// 目标 RowId。
        rowid: u64,
        /// 字段覆盖。
        fields: UpdateFields,
    },
    /// 访问统计/重要度增量。
    Access {
        /// 写序号。
        seqno: u64,
        /// 事务时间。
        tx_ms: i64,
        /// 命名空间编号。
        ns_id: u32,
        /// 目标 RowId。
        rowid: u64,
        /// 最近访问时刻。
        last_access_ms: i64,
        /// 访问次数增量。
        access_delta: u32,
        /// 重要度增量。
        importance_delta: f32,
    },
    /// 建立关系边。
    Relate {
        /// 写序号。
        seqno: u64,
        /// 事务时间。
        tx_ms: i64,
        /// 命名空间编号。
        ns_id: u32,
        /// 起点。
        from: u64,
        /// 终点。
        to: u64,
        /// 关系类型。
        kind: u16,
        /// 边权。
        weight: f32,
        /// 边元数据。
        meta: Meta,
    },
    /// 删除关系边。
    Unrelate {
        /// 写序号。
        seqno: u64,
        /// 事务时间。
        tx_ms: i64,
        /// 命名空间编号。
        ns_id: u32,
        /// 起点。
        from: u64,
        /// 终点。
        to: u64,
        /// 关系类型。
        kind: u16,
    },
}

const KIND_DELETE_KEY: u8 = 1;
const KIND_DELETE_ROW: u8 = 2;
const KIND_UPDATE_ROW: u8 = 3;
const KIND_ACCESS: u8 = 4;
const KIND_RELATE: u8 = 5;
const KIND_UNRELATE: u8 = 6;

/// 编码 delta 区(空切片等价于无覆盖,返回空 `Vec`)。
pub(crate) fn encode(entries: &[DeltaEntry]) -> Vec<u8> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mut body = Vec::new();
    for entry in entries {
        encode_entry(&mut body, entry);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    put_u16(&mut out, FORMAT_VERSION);
    put_u16(&mut out, entries.len() as u16);
    put_u32(&mut out, crc32(&body));
    out.extend_from_slice(&body);
    out
}

/// 编码一条 delta 条目到 `out`。
fn encode_entry(out: &mut Vec<u8>, entry: &DeltaEntry) {
    match entry {
        DeltaEntry::DeleteKey {
            seqno,
            tx_ms,
            ns_id,
            key,
        } => {
            put_header(out, KIND_DELETE_KEY, *seqno, *tx_ms, *ns_id);
            put_bytes_u32(out, key.as_bytes());
        }
        DeltaEntry::DeleteRow {
            seqno,
            tx_ms,
            ns_id,
            rowid,
        } => {
            put_header(out, KIND_DELETE_ROW, *seqno, *tx_ms, *ns_id);
            put_u64(out, *rowid);
        }
        DeltaEntry::UpdateRow {
            seqno,
            tx_ms,
            ns_id,
            rowid,
            fields,
        } => {
            put_header(out, KIND_UPDATE_ROW, *seqno, *tx_ms, *ns_id);
            put_u64(out, *rowid);
            let mask = update_mask_of(fields);
            out.push(mask);
            encode_update_fields(out, fields, mask);
        }
        DeltaEntry::Access {
            seqno,
            tx_ms,
            ns_id,
            rowid,
            last_access_ms,
            access_delta,
            importance_delta,
        } => {
            put_header(out, KIND_ACCESS, *seqno, *tx_ms, *ns_id);
            put_u64(out, *rowid);
            put_i64(out, *last_access_ms);
            put_u32(out, *access_delta);
            out.extend_from_slice(&importance_delta.to_le_bytes());
        }
        DeltaEntry::Relate {
            seqno,
            tx_ms,
            ns_id,
            from,
            to,
            kind,
            weight,
            meta,
        } => {
            put_header(out, KIND_RELATE, *seqno, *tx_ms, *ns_id);
            put_u64(out, *from);
            put_u64(out, *to);
            put_u16(out, *kind);
            out.extend_from_slice(&weight.to_le_bytes());
            put_bytes_u32(out, &meta::to_bytes(meta));
        }
        DeltaEntry::Unrelate {
            seqno,
            tx_ms,
            ns_id,
            from,
            to,
            kind,
        } => {
            put_header(out, KIND_UNRELATE, *seqno, *tx_ms, *ns_id);
            put_u64(out, *from);
            put_u64(out, *to);
            put_u16(out, *kind);
        }
    }
}

/// 写入公共条目头 `[kind][seqno][tx_ms][ns_id]`。
fn put_header(out: &mut Vec<u8>, kind: u8, seqno: u64, tx_ms: i64, ns_id: u32) {
    out.push(kind);
    put_u64(out, seqno);
    put_i64(out, tx_ms);
    put_u32(out, ns_id);
}

/// 计算 `UpdateFields` 的字段掩码。
fn update_mask_of(fields: &UpdateFields) -> u8 {
    use update_mask::*;
    let mut mask = 0_u8;
    if fields.text.is_some() {
        mask |= TEXT;
    }
    if fields.meta.is_some() {
        mask |= META;
    }
    if fields.expires_at_ms.is_some() {
        mask |= TTL;
    }
    if fields.importance.is_some() {
        mask |= IMPORTANCE;
    }
    if fields.confidence.is_some() {
        mask |= CONFIDENCE;
    }
    if fields.valid_time.is_some() {
        mask |= VALID_TIME;
    }
    mask
}

/// 按掩码编码 `UpdateFields` 的可选字段。
fn encode_update_fields(out: &mut Vec<u8>, fields: &UpdateFields, mask: u8) {
    use update_mask::*;
    if mask & TEXT != 0 {
        match &fields.text {
            Some(Some(text)) => {
                out.push(1);
                put_bytes_u32(out, text.as_bytes());
            }
            _ => out.push(0),
        }
    }
    if mask & META != 0 {
        match &fields.meta {
            Some(Some(meta)) => {
                out.push(1);
                put_bytes_u32(out, &meta::to_bytes(meta));
            }
            _ => out.push(0),
        }
    }
    if mask & TTL != 0 {
        match fields.expires_at_ms {
            Some(Some(expires)) => {
                out.push(1);
                put_i64(out, expires);
            }
            _ => out.push(0),
        }
    }
    if mask & IMPORTANCE != 0
        && let Some(importance) = fields.importance
    {
        out.extend_from_slice(&importance.to_le_bytes());
    }
    if mask & CONFIDENCE != 0
        && let Some(confidence) = fields.confidence
    {
        out.extend_from_slice(&confidence.to_le_bytes());
    }
    if mask & VALID_TIME != 0 {
        match &fields.valid_time {
            Some(Some((valid_from, valid_to))) => {
                out.push(1);
                put_i64(out, *valid_from);
                match valid_to {
                    Some(valid_to) => {
                        out.push(1);
                        put_i64(out, *valid_to);
                    }
                    None => out.push(0),
                }
            }
            _ => out.push(0),
        }
    }
}

/// 校验并解析 delta 区;空切片返回空条目。
///
/// # Errors
/// 魔数/版本/CRC 不符、未知 kind 或长度越界时返回结构化错误。
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<DeltaEntry>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut cursor = Cursor::new(bytes, "delta");
    if cursor.take(4)? != MAGIC {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "delta: 魔数不符".to_string(),
        });
    }
    check_version("delta", cursor.u16()?)?;
    let count = cursor.u16()? as usize;
    let stored_crc = cursor.u32()?;
    let body_start = cursor.position();
    if crc32(&bytes[body_start..]) != stored_crc {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "delta: delta_crc32 不符".to_string(),
        });
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        entries.push(decode_entry(&mut cursor)?);
    }
    Ok(entries)
}

/// 解码单条 delta 条目。
fn decode_entry(cursor: &mut Cursor<'_>) -> Result<DeltaEntry> {
    let kind = cursor.u8()?;
    let seqno = cursor.u64()?;
    let tx_ms = cursor.i64()?;
    let ns_id = cursor.u32()?;
    match kind {
        KIND_DELETE_KEY => {
            let len = cursor.u32()? as usize;
            let key =
                std::str::from_utf8(cursor.take(len)?).map_err(|error| MnemeError::Corrupted {
                    segment: None,
                    reason: format!("delta: key 非 UTF-8:{error}"),
                })?;
            Ok(DeltaEntry::DeleteKey {
                seqno,
                tx_ms,
                ns_id,
                key: Arc::from(key),
            })
        }
        KIND_DELETE_ROW => Ok(DeltaEntry::DeleteRow {
            seqno,
            tx_ms,
            ns_id,
            rowid: cursor.u64()?,
        }),
        KIND_UPDATE_ROW => {
            let rowid = cursor.u64()?;
            let mask = cursor.u8()?;
            let fields = decode_update_fields(cursor, mask)?;
            Ok(DeltaEntry::UpdateRow {
                seqno,
                tx_ms,
                ns_id,
                rowid,
                fields,
            })
        }
        KIND_ACCESS => Ok(DeltaEntry::Access {
            seqno,
            tx_ms,
            ns_id,
            rowid: cursor.u64()?,
            last_access_ms: cursor.i64()?,
            access_delta: cursor.u32()?,
            importance_delta: f32::from_le_bytes(cursor.take(4)?.try_into().unwrap_or([0; 4])),
        }),
        KIND_RELATE => {
            let from = cursor.u64()?;
            let to = cursor.u64()?;
            let kind = cursor.u16()?;
            let weight = f32::from_le_bytes(cursor.take(4)?.try_into().unwrap_or([0; 4]));
            let meta_len = cursor.u32()? as usize;
            let meta = meta::from_bytes(cursor.take(meta_len)?)?;
            Ok(DeltaEntry::Relate {
                seqno,
                tx_ms,
                ns_id,
                from,
                to,
                kind,
                weight,
                meta,
            })
        }
        KIND_UNRELATE => Ok(DeltaEntry::Unrelate {
            seqno,
            tx_ms,
            ns_id,
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

/// 按掩码解码 `UpdateFields`。
fn decode_update_fields(cursor: &mut Cursor<'_>, mask: u8) -> Result<UpdateFields> {
    use update_mask::*;
    let mut fields = UpdateFields::default();
    if mask & TEXT != 0 {
        fields.text = if cursor.u8()? != 0 {
            let len = cursor.u32()? as usize;
            let text =
                std::str::from_utf8(cursor.take(len)?).map_err(|error| MnemeError::Corrupted {
                    segment: None,
                    reason: format!("delta: text 非 UTF-8:{error}"),
                })?;
            Some(Some(Arc::from(text)))
        } else {
            Some(None)
        };
    }
    if mask & META != 0 {
        fields.meta = if cursor.u8()? != 0 {
            let len = cursor.u32()? as usize;
            Some(Some(meta::from_bytes(cursor.take(len)?)?))
        } else {
            Some(None)
        };
    }
    if mask & TTL != 0 {
        fields.expires_at_ms = if cursor.u8()? != 0 {
            Some(Some(cursor.i64()?))
        } else {
            Some(None)
        };
    }
    if mask & IMPORTANCE != 0 {
        fields.importance = Some(f32::from_le_bytes(
            cursor.take(4)?.try_into().unwrap_or([0; 4]),
        ));
    }
    if mask & CONFIDENCE != 0 {
        fields.confidence = Some(f32::from_le_bytes(
            cursor.take(4)?.try_into().unwrap_or([0; 4]),
        ));
    }
    if mask & VALID_TIME != 0 {
        fields.valid_time = if cursor.u8()? != 0 {
            let valid_from = cursor.i64()?;
            let has_to = cursor.u8()? != 0;
            let valid_to = if has_to { Some(cursor.i64()?) } else { None };
            Some(Some((valid_from, valid_to)))
        } else {
            Some(None)
        };
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::meta::json;

    fn delete_key(seqno: u64, key: &str) -> DeltaEntry {
        DeltaEntry::DeleteKey {
            seqno,
            tx_ms: 100,
            ns_id: 1,
            key: Arc::from(key),
        }
    }

    /// 六种条目往返一致。
    #[test]
    fn delta_roundtrip_all_kinds() {
        let entries = vec![
            delete_key(1, "alpha"),
            DeltaEntry::DeleteRow {
                seqno: 2,
                tx_ms: 100,
                ns_id: 1,
                rowid: 7,
            },
            DeltaEntry::UpdateRow {
                seqno: 3,
                tx_ms: 100,
                ns_id: 1,
                rowid: 7,
                fields: UpdateFields {
                    text: Some(Some(Arc::from("new"))),
                    meta: Some(Some(json!({"a": 1}))),
                    expires_at_ms: Some(None),
                    importance: Some(0.8),
                    confidence: Some(0.5),
                    valid_time: Some(Some((1, Some(2)))),
                },
            },
            DeltaEntry::Access {
                seqno: 4,
                tx_ms: 100,
                ns_id: 1,
                rowid: 7,
                last_access_ms: 999,
                access_delta: 2,
                importance_delta: 0.1,
            },
            DeltaEntry::Relate {
                seqno: 5,
                tx_ms: 100,
                ns_id: 1,
                from: 1,
                to: 2,
                kind: 3,
                weight: 0.5,
                meta: json!({"m": true}),
            },
            DeltaEntry::Unrelate {
                seqno: 6,
                tx_ms: 100,
                ns_id: 1,
                from: 1,
                to: 2,
                kind: 3,
            },
        ];
        let bytes = encode(&entries);
        let decoded = parse(&bytes).expect("parse");
        assert_eq!(decoded, entries);
    }

    /// 空 delta 与无覆盖等价。
    #[test]
    fn delta_empty_roundtrip() {
        assert!(encode(&[]).is_empty());
        assert!(parse(&[]).expect("parse").is_empty());
    }

    /// CRC 损坏被检出。
    #[test]
    fn delta_detects_corruption() {
        let mut bytes = encode(&[delete_key(1, "a")]);
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// 未知 kind 报错、不静默跳过。
    #[test]
    fn delta_rejects_unknown_kind() {
        let mut bytes = encode(&[delete_key(1, "a")]);
        // kind 位于 magic/ver/count/crc 之后的首字节。
        bytes[12] = 0x7F;
        // 重新计算 CRC 使未知 kind 成为唯一错误来源。
        let body_start = 12;
        let crc = crc32(&bytes[body_start..]);
        bytes[8..12].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }
}
