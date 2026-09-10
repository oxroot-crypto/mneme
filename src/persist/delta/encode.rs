//! delta 条目编码与字段掩码辅助(`delta/encode.rs`)。

use std::sync::Arc;

use crate::core::meta::{self, Meta};
use crate::persist::{put_bytes_u32, put_i64, put_u16, put_u32, put_u64};

use super::update_mask;
use super::{
    DeltaEntry, EntryHead, KIND_ACCESS, KIND_DELETE_KEY, KIND_DELETE_ROW, KIND_RELATE,
    KIND_UNRELATE, KIND_UPDATE_ROW, UpdateFields,
};

/// 编码一条 delta 条目到 `out`。
pub(super) fn encode_entry(out: &mut Vec<u8>, entry: &DeltaEntry) {
    put_header(out, entry_kind(entry), entry_head(entry));
    match entry {
        DeltaEntry::DeleteKey { key, .. } => put_bytes_u32(out, key.as_bytes()),
        DeltaEntry::DeleteRow { rowid, .. } => put_u64(out, *rowid),
        DeltaEntry::UpdateRow { rowid, fields, .. } => {
            put_u64(out, *rowid);
            let mask = update_mask_of(fields);
            out.push(mask);
            encode_update_fields(out, fields, mask);
        }
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

/// 条目的 kind 编号。
fn entry_kind(entry: &DeltaEntry) -> u8 {
    match entry {
        DeltaEntry::DeleteKey { .. } => KIND_DELETE_KEY,
        DeltaEntry::DeleteRow { .. } => KIND_DELETE_ROW,
        DeltaEntry::UpdateRow { .. } => KIND_UPDATE_ROW,
        DeltaEntry::Access { .. } => KIND_ACCESS,
        DeltaEntry::Relate { .. } => KIND_RELATE,
        DeltaEntry::Unrelate { .. } => KIND_UNRELATE,
    }
}

/// 条目的公共头字段。
fn entry_head(entry: &DeltaEntry) -> EntryHead {
    match entry {
        DeltaEntry::DeleteKey {
            seqno,
            tx_ms,
            ns_id,
            ..
        }
        | DeltaEntry::DeleteRow {
            seqno,
            tx_ms,
            ns_id,
            ..
        }
        | DeltaEntry::UpdateRow {
            seqno,
            tx_ms,
            ns_id,
            ..
        }
        | DeltaEntry::Access {
            seqno,
            tx_ms,
            ns_id,
            ..
        }
        | DeltaEntry::Relate {
            seqno,
            tx_ms,
            ns_id,
            ..
        }
        | DeltaEntry::Unrelate {
            seqno,
            tx_ms,
            ns_id,
            ..
        } => EntryHead {
            seqno: *seqno,
            tx_ms: *tx_ms,
            ns_id: *ns_id,
        },
    }
}

/// 写入公共条目头 `[kind][seqno][tx_ms][ns_id]`。
fn put_header(out: &mut Vec<u8>, kind: u8, head: EntryHead) {
    out.push(kind);
    put_u64(out, head.seqno);
    put_i64(out, head.tx_ms);
    put_u32(out, head.ns_id);
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
        encode_optional_str(out, fields.text.as_ref());
    }
    if mask & META != 0 {
        encode_optional_meta(out, fields.meta.as_ref());
    }
    if mask & TTL != 0 {
        encode_optional_i64(out, fields.expires_at_ms);
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
        encode_optional_valid_time(out, fields.valid_time.as_ref());
    }
}

/// 编码可清除的文本覆盖:存在时写 `1` + 串,清除时写 `0`。
fn encode_optional_str(out: &mut Vec<u8>, value: Option<&Option<Arc<str>>>) {
    match value {
        Some(Some(text)) => {
            out.push(1);
            put_bytes_u32(out, text.as_bytes());
        }
        _ => out.push(0),
    }
}

/// 编码可清除的元数据覆盖。
fn encode_optional_meta(out: &mut Vec<u8>, value: Option<&Option<Meta>>) {
    match value {
        Some(Some(meta)) => {
            out.push(1);
            put_bytes_u32(out, &meta::to_bytes(meta));
        }
        _ => out.push(0),
    }
}

/// 编码可清除的 `i64` 覆盖。
fn encode_optional_i64(out: &mut Vec<u8>, value: Option<Option<i64>>) {
    match value {
        Some(Some(value)) => {
            out.push(1);
            put_i64(out, value);
        }
        _ => out.push(0),
    }
}

/// 编码可清除的有效时间覆盖 `(valid_from, Option<valid_to>)`。
fn encode_optional_valid_time(out: &mut Vec<u8>, value: Option<&Option<(i64, Option<i64>)>>) {
    match value {
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
