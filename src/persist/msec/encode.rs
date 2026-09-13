//! msec 段与记录体编码(`msec/encode.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta;
use crate::persist::{FORMAT_VERSION, align_up, crc32, put_bytes_u32, put_i64, put_u32, put_u64};

use super::{
    EntryData, FLAG_ACCESS, FLAG_CONFIDENCE, FLAG_IMPORTANCE, FLAG_KEY, FLAG_PROVENANCE, FLAG_TEXT,
    FLAG_TTL, FLAG_VALID_TIME, HEADER_CRC_COVER, HEADER_CRC_OFFSET, HEADER_LEN, KeyIndexRow, MAGIC,
    MsecInput, NS_STAT_ROW_BYTES, NsStatRow, REGION_ALIGN, Region, Regions, SlotMeta,
    TOMBSTONE_DOC_OFFSET, VERSION_ROW_BYTES, VersionRow, region_offset,
};

/// 编码一个完整的 msec 文件。
///
/// # Errors
/// 槽位记录体长度超限或编码失败时返回结构化错误。
pub(crate) fn encode(input: &MsecInput<'_>) -> Result<Vec<u8>> {
    let (doc_region, slot_offsets) = build_doc_region(input.slots)?;
    let version_table = build_version_table(input.slots, &slot_offsets);
    let key_rows = build_key_rows(input.slots, &slot_offsets);
    let (body, regions) = assemble_regions(input, doc_region, &version_table, &key_rows);
    let header = encode_header(input.slots.len() as u64, regions);
    let mut out = header.to_vec();
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_le_bytes());
    Ok(out)
}

/// 顺序写入非墓碑记录体,单遍记录每个槽位的偏移(避免 O(n²) 重编码)。
fn build_doc_region(slots: &[SlotMeta]) -> Result<(Vec<u8>, Vec<u64>)> {
    let mut doc_region = Vec::new();
    let mut offsets = Vec::with_capacity(slots.len());
    for slot in slots {
        let offset = match &slot.body {
            Some(body) => {
                let offset = doc_region.len() as u64;
                doc_region.extend_from_slice(&encode_entry(body)?);
                offset
            }
            None => TOMBSTONE_DOC_OFFSET,
        };
        offsets.push(offset);
    }
    Ok((doc_region, offsets))
}

/// 构造版本链表(按 `(rowid, seqno)` 排序)。
fn build_version_table(slots: &[SlotMeta], offsets: &[u64]) -> Vec<VersionRow> {
    let mut rows: Vec<VersionRow> = slots
        .iter()
        .zip(offsets)
        .enumerate()
        .map(|(index, (slot, offset))| VersionRow {
            rowid: slot.rowid.get(),
            seqno: slot.seqno.get(),
            tx_ms: slot.tx_ms,
            slot_id: index as u32,
            doc_offset: *offset,
        })
        .collect();
    rows.sort_by_key(|row| (row.rowid, row.seqno));
    rows
}

/// 构造 key 索引(仅带 key 的非墓碑槽位,按 `(NsId, key)` 排序)。
fn build_key_rows(slots: &[SlotMeta], offsets: &[u64]) -> Vec<KeyIndexRow> {
    let mut rows = Vec::new();
    for (index, (slot, offset)) in slots.iter().zip(offsets).enumerate() {
        let Some(body) = &slot.body else { continue };
        let Some(key) = &body.key else { continue };
        rows.push(KeyIndexRow {
            ns_id: body.ns_id.get(),
            key: Arc::from(key.as_str()),
            rowid: body.rowid.get(),
            slot_id: index as u32,
            seqno: body.seqno.get(),
            doc_offset: *offset,
        });
    }
    rows.sort_by(|a, b| (a.ns_id, a.key.as_ref()).cmp(&(b.ns_id, b.key.as_ref())));
    rows
}

/// 组装数据区并记录各区偏移。
///
/// `doc_region` 置于数据区首位,故 `version_table.doc_offset`(相对 doc_region 起点)
/// 即相对数据区起点。
fn assemble_regions(
    input: &MsecInput<'_>,
    doc_region: Vec<u8>,
    version_table: &[VersionRow],
    key_rows: &[KeyIndexRow],
) -> (Vec<u8>, Regions) {
    let mut body = doc_region;
    let field_dict = append_region(&mut body, input.field_dict);
    let version = append_region(&mut body, &encode_version_table(version_table));
    let key = append_region(&mut body, &encode_key_index(key_rows));
    let inv = append_region(&mut body, input.inverted);
    let ns_stats = append_region(&mut body, &encode_ns_stats(input.ns_stats));
    let zmap = append_region(&mut body, input.zmap);
    let bloom = append_region(&mut body, input.bloom);
    let delta = append_region(&mut body, input.delta);
    let rel = append_region(&mut body, input.relations);
    (
        body,
        Regions {
            field_dict,
            version,
            key,
            inv,
            ns_stats,
            zmap,
            bloom,
            delta,
            rel,
        },
    )
}

/// 追加一个数据区到 `body`(起点对齐 8 B),返回其**文件绝对**偏移/长度。
fn append_region(body: &mut Vec<u8>, bytes: &[u8]) -> Region {
    let aligned = align_up(body.len(), REGION_ALIGN);
    body.resize(aligned, 0);
    let offset = HEADER_LEN as u64 + body.len() as u64;
    body.extend_from_slice(bytes);
    Region {
        offset,
        len: bytes.len() as u64,
    }
}

/// 编码 192 字节头部(CRC 覆盖 `[0,160)`)。
fn encode_header(row_count: u64, regions: Regions) -> [u8; HEADER_LEN as usize] {
    let mut out = [0_u8; HEADER_LEN as usize];
    out[0..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    out[6..8].copy_from_slice(&HEADER_LEN.to_le_bytes());
    out[8..16].copy_from_slice(&row_count.to_le_bytes());
    let mut put_pair = |index: usize, region: Region| {
        out[index..index + 8].copy_from_slice(&region.offset.to_le_bytes());
        out[index + 8..index + 16].copy_from_slice(&region.len.to_le_bytes());
    };
    // 按设计顺序:field_dict(16)、version(32)、key(48)、inv(64)、ns_stats(80)、
    // zmap(96)、bloom(112)、delta(128)、rel(144)。
    put_pair(region_offset(0), regions.field_dict);
    put_pair(region_offset(1), regions.version);
    put_pair(region_offset(2), regions.key);
    put_pair(region_offset(3), regions.inv);
    put_pair(region_offset(4), regions.ns_stats);
    put_pair(region_offset(5), regions.zmap);
    put_pair(region_offset(6), regions.bloom);
    put_pair(region_offset(7), regions.delta);
    put_pair(region_offset(8), regions.rel);
    let crc = crc32(&out[0..HEADER_CRC_COVER]);
    out[HEADER_CRC_OFFSET..HEADER_CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
    out
}

/// 编码记录体(含 `u32 total_len` 前缀);WAL `Insert` 帧复用此格式。
///
/// # Errors
/// 记录体长度超出 `u32` 时返回 [`MnemeError::TooLarge`]。
pub(crate) fn encode_entry(entry: &EntryData) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    put_u64(&mut payload, entry.rowid.get());
    put_u64(&mut payload, entry.seqno.get());
    put_u32(&mut payload, entry.ns_id.get());
    payload.push(entry_flags(entry));
    encode_entry_fields(&mut payload, entry);

    let total_len = u32::try_from(payload.len()).map_err(|_| MnemeError::TooLarge {
        field: "msec entry",
        limit: u32::MAX as usize,
        got: payload.len(),
    })?;
    let mut out = Vec::with_capacity(payload.len() + 4);
    put_u32(&mut out, total_len);
    out.extend_from_slice(&payload);
    Ok(out)
}

/// 计算记录体的 flags 字节。
fn entry_flags(entry: &EntryData) -> u8 {
    let mut flags = 0_u8;
    if entry.key.is_some() {
        flags |= FLAG_KEY;
    }
    if entry.text.is_some() {
        flags |= FLAG_TEXT;
    }
    if entry.expires_at_ms.is_some() {
        flags |= FLAG_TTL;
    }
    if entry.importance.is_some() {
        flags |= FLAG_IMPORTANCE;
    }
    if entry.access.is_some() {
        flags |= FLAG_ACCESS;
    }
    if entry.valid_time.is_some() {
        flags |= FLAG_VALID_TIME;
    }
    if entry.confidence.is_some() {
        flags |= FLAG_CONFIDENCE;
    }
    if entry.provenance.is_some() {
        flags |= FLAG_PROVENANCE;
    }
    flags
}

/// 按 flags 写入记录体的可选字段(不含固定头与 flags)。
fn encode_entry_fields(payload: &mut Vec<u8>, entry: &EntryData) {
    if let Some(key) = &entry.key {
        put_bytes_u32(payload, key.as_str().as_bytes());
    }
    if let Some(text) = &entry.text {
        put_bytes_u32(payload, text.as_bytes());
    }
    put_bytes_u32(payload, &meta::to_bytes(&entry.meta));
    put_i64(payload, entry.created_at_ms);
    if let Some(expires) = entry.expires_at_ms {
        put_i64(payload, expires);
    }
    if let Some(importance) = entry.importance {
        payload.extend_from_slice(&importance.to_le_bytes());
    }
    if let Some((last_access, count)) = entry.access {
        put_i64(payload, last_access);
        put_u32(payload, count);
    }
    if let Some((valid_from, valid_to)) = entry.valid_time {
        put_i64(payload, valid_from);
        match valid_to {
            Some(valid_to) => {
                payload.push(1);
                put_i64(payload, valid_to);
            }
            None => payload.push(0),
        }
    }
    if let Some(confidence) = entry.confidence {
        payload.extend_from_slice(&confidence.to_le_bytes());
    }
    if let Some(provenance) = &entry.provenance {
        put_bytes_u32(payload, &meta::to_bytes(provenance));
    }
}

/// 编码版本链表(定长 36 B/行)。
fn encode_version_table(rows: &[VersionRow]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rows.len() * VERSION_ROW_BYTES);
    for row in rows {
        put_u64(&mut out, row.rowid);
        put_u64(&mut out, row.seqno);
        put_i64(&mut out, row.tx_ms);
        put_u32(&mut out, row.slot_id);
        put_u64(&mut out, row.doc_offset);
    }
    out
}

/// 编码 key 索引(变长)。
fn encode_key_index(rows: &[KeyIndexRow]) -> Vec<u8> {
    let mut out = Vec::new();
    for row in rows {
        put_u32(&mut out, row.ns_id);
        put_bytes_u32(&mut out, row.key.as_bytes());
        put_u64(&mut out, row.rowid);
        put_u32(&mut out, row.slot_id);
        put_u64(&mut out, row.seqno);
        put_u64(&mut out, row.doc_offset);
    }
    out
}

/// 编码命名空间统计(定长 20 B/行)。
fn encode_ns_stats(rows: &[NsStatRow]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rows.len() * NS_STAT_ROW_BYTES);
    for row in rows {
        put_u32(&mut out, row.ns_id);
        put_u64(&mut out, row.doc_count);
        put_u64(&mut out, row.total_doc_len);
    }
    out
}
