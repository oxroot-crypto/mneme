//! 元数据段(`msec`)编解码(设计 04 §2.2、§5.5)。
//!
//! 文件 = 定长头(160 B 字段 + CRC,补齐到 192 B)+ 变长数据区 + 尾部 payload CRC。
//! 头部用 8 组 `offset/len` 指向各数据区;L2 落地 `doc_region`(记录体)、
//! `version_table`(版本链)、`key_index`、`ns_stats`、`delta`、`relations`;
//! `field_dict`/`zone_maps`/`ttl_map`/`blooms`/`inverted` 属 L4,本层写空(`len=0`)。
//!
//! **记录体(entry)**:`[u32 total_len][u64 rowid][u64 seqno][u32 ns_id][u8 flags]`
//! 之后按 flags 依次出现可选的 key/text、meta、时间与统计字段。墓碑版本无记录体,
//! 其 `doc_offset` 记为 [`TOMBSTONE_DOC_OFFSET`](`u64::MAX`),由 vsec 删除位图标为不可见。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::{self, Meta};
use crate::core::types::{Key, NsId, RowId, SeqNo};
use crate::persist::{
    Cursor, FORMAT_VERSION, align_up, check_version, crc32, put_bytes_u32, put_i64, put_u32,
    put_u64,
};

/// 元数据段魔数。
pub(crate) const MAGIC: [u8; 4] = *b"MSC1";
/// 定长头部长度(字节):160 B 字段 + 4 B CRC,补齐到 64B 对齐。
pub(crate) const HEADER_LEN: u16 = 192;
/// 头部 CRC 覆盖的字节数(CRC 字段之前)。
const HEADER_CRC_COVER: usize = 160;
/// 墓碑版本在 `version_table` 中的 `doc_offset` 哨兵(无记录体)。
pub(crate) const TOMBSTONE_DOC_OFFSET: u64 = u64::MAX;

const FLAG_KEY: u8 = 1 << 0;
const FLAG_TEXT: u8 = 1 << 1;
const FLAG_TTL: u8 = 1 << 2;
const FLAG_IMPORTANCE: u8 = 1 << 3;
const FLAG_ACCESS: u8 = 1 << 4;
const FLAG_VALID_TIME: u8 = 1 << 5;
const FLAG_CONFIDENCE: u8 = 1 << 6;
const FLAG_PROVENANCE: u8 = 1 << 7;

/// 一条记录的元数据(不含向量;向量在 vsec)。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EntryData {
    /// 稳定逻辑标识。
    pub(crate) rowid: RowId,
    /// 全局写序号。
    pub(crate) seqno: SeqNo,
    /// 命名空间编号。
    pub(crate) ns_id: NsId,
    /// 可选业务 key。
    pub(crate) key: Option<Key>,
    /// 可选文本。
    pub(crate) text: Option<Arc<str>>,
    /// 元数据 JSON。
    pub(crate) meta: Meta,
    /// 事务时间(Unix 毫秒)。
    pub(crate) created_at_ms: i64,
    /// 逻辑过期时间(可选)。
    pub(crate) expires_at_ms: Option<i64>,
    /// 重要度(可选,缺省 0.5)。
    pub(crate) importance: Option<f32>,
    /// 访问统计 `(last_access_ms, access_count)`(可选)。
    pub(crate) access: Option<(i64, u32)>,
    /// 有效时间 `(valid_from_ms, valid_to_ms)`(可选)。
    pub(crate) valid_time: Option<(i64, Option<i64>)>,
    /// 可信度(可选,缺省 1.0)。
    pub(crate) confidence: Option<f32>,
    /// 来源/派生链(可选)。
    pub(crate) provenance: Option<Meta>,
}

/// 段内一个物理槽位:版本元信息 + 可选记录体(`None` = 墓碑)。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SlotMeta {
    /// 稳定逻辑标识。
    pub(crate) rowid: RowId,
    /// 全局写序号。
    pub(crate) seqno: SeqNo,
    /// 事务时间(Unix 毫秒)。
    pub(crate) tx_ms: i64,
    /// 记录体;墓碑为 `None`。
    pub(crate) body: Option<EntryData>,
}

/// `version_table` 行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VersionRow {
    /// 稳定逻辑标识。
    pub(crate) rowid: u64,
    /// 全局写序号。
    pub(crate) seqno: u64,
    /// 事务时间(Unix 毫秒)。
    pub(crate) tx_ms: i64,
    /// 物理槽位下标。
    pub(crate) slot_id: u32,
    /// 记录体在 `doc_region` 中的字节偏移;墓碑为 [`TOMBSTONE_DOC_OFFSET`]。
    pub(crate) doc_offset: u64,
}

/// `key_index` 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyIndexRow {
    /// 命名空间编号。
    pub(crate) ns_id: u32,
    /// 业务 key。
    pub(crate) key: Arc<str>,
    /// 稳定逻辑标识。
    pub(crate) rowid: u64,
    /// 物理槽位下标。
    pub(crate) slot_id: u32,
    /// 全局写序号。
    pub(crate) seqno: u64,
    /// 记录体在 `doc_region` 中的字节偏移。
    pub(crate) doc_offset: u64,
}

/// 命名空间级统计行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NsStatRow {
    /// 命名空间编号。
    pub(crate) ns_id: u32,
    /// 活行数。
    pub(crate) doc_count: u64,
    /// 文本总长度。
    pub(crate) total_doc_len: u64,
}

/// msec 编码输入。
pub(crate) struct MsecInput<'a> {
    /// 段内物理槽位(顺序即 `SlotId`,墓碑 `body = None`)。
    pub(crate) slots: &'a [SlotMeta],
    /// 命名空间统计。
    pub(crate) ns_stats: &'a [NsStatRow],
    /// 预编码的 delta 区(见 [`crate::persist::delta`]);无覆盖时为 `&[]`。
    pub(crate) delta: &'a [u8],
    /// 预编码的 relations 区(见 [`crate::persist::edges`]);无边时为 `&[]`。
    pub(crate) relations: &'a [u8],
}

/// msec 各数据区偏移/长度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Region {
    offset: u64,
    len: u64,
}

/// 编码一个完整的 msec 文件。
///
/// # Errors
/// 槽位记录体长度超限或编码失败时返回结构化错误。
pub(crate) fn encode(input: &MsecInput<'_>) -> Result<Vec<u8>> {
    // 1) doc_region:顺序写入非墓碑记录体,单遍记录每个槽位的偏移(避免 O(n²) 重编码)。
    let mut doc_region = Vec::new();
    let mut slot_offsets = Vec::with_capacity(input.slots.len());
    let mut version_table = Vec::with_capacity(input.slots.len());
    for (slot_index, slot) in input.slots.iter().enumerate() {
        let doc_offset = match &slot.body {
            Some(body) => {
                let offset = doc_region.len() as u64;
                doc_region.extend_from_slice(&encode_entry(body)?);
                offset
            }
            None => TOMBSTONE_DOC_OFFSET,
        };
        slot_offsets.push(doc_offset);
        version_table.push(VersionRow {
            rowid: slot.rowid.get(),
            seqno: slot.seqno.get(),
            tx_ms: slot.tx_ms,
            slot_id: slot_index as u32,
            doc_offset,
        });
    }
    version_table.sort_by_key(|row| (row.rowid, row.seqno));

    // 2) key_index:仅带 key 的非墓碑槽位,按 (NsId, key) 排序。
    let mut key_rows = Vec::new();
    for (slot_index, slot) in input.slots.iter().enumerate() {
        let Some(body) = &slot.body else { continue };
        let Some(key) = &body.key else { continue };
        key_rows.push(KeyIndexRow {
            ns_id: body.ns_id.get(),
            key: Arc::from(key.as_str()),
            rowid: body.rowid.get(),
            slot_id: slot_index as u32,
            seqno: body.seqno.get(),
            doc_offset: slot_offsets[slot_index],
        });
    }
    key_rows.sort_by(|a, b| (a.ns_id, a.key.as_ref()).cmp(&(b.ns_id, b.key.as_ref())));

    // 3) 组装数据区并记录各区偏移。`doc_region` 置于数据区首位,
    //    故 `version_table.doc_offset`(相对 doc_region 起点)即相对数据区起点。
    let mut data = doc_region;
    let version = append_region(&mut data, &encode_version_table(&version_table));
    let key = append_region(&mut data, &encode_key_index(&key_rows));
    let inv = append_region(&mut data, &[]); // 倒排索引:L4
    let ns_stats = append_region(&mut data, &encode_ns_stats(input.ns_stats));
    let zmap = append_region(&mut data, &[]); // zone map / ttl map:L4
    let bloom = append_region(&mut data, &[]); // bloom:L4
    let delta = append_region(&mut data, input.delta);
    let rel = append_region(&mut data, input.relations);

    // 4) 头部。`doc_region` 无独立头字段:经 `version_table.doc_offset` 定位。
    let header = encode_header(
        input.slots.len() as u64,
        version,
        key,
        inv,
        ns_stats,
        zmap,
        bloom,
        delta,
        rel,
    );
    let mut out = header.to_vec();
    out.extend_from_slice(&data);
    out.extend_from_slice(&crc32(&data).to_le_bytes());
    Ok(out)
}

/// 追加一个数据区到 `data`(起点对齐 8 B),返回其**文件绝对**偏移/长度。
fn append_region(data: &mut Vec<u8>, bytes: &[u8]) -> Region {
    let aligned = align_up(data.len(), 8);
    data.resize(aligned, 0);
    let offset = HEADER_LEN as u64 + data.len() as u64;
    data.extend_from_slice(bytes);
    Region {
        offset,
        len: bytes.len() as u64,
    }
}

/// 编码 192 字节头部(CRC 覆盖 `[0,160)`)。
#[allow(clippy::too_many_arguments)]
fn encode_header(
    row_count: u64,
    version: Region,
    key: Region,
    inv: Region,
    ns_stats: Region,
    zmap: Region,
    bloom: Region,
    delta: Region,
    rel: Region,
) -> [u8; HEADER_LEN as usize] {
    let mut out = [0_u8; HEADER_LEN as usize];
    out[0..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    out[6..8].copy_from_slice(&HEADER_LEN.to_le_bytes());
    out[8..16].copy_from_slice(&row_count.to_le_bytes());
    let mut put_pair = |index: usize, region: Region| {
        out[index..index + 8].copy_from_slice(&region.offset.to_le_bytes());
        out[index + 8..index + 16].copy_from_slice(&region.len.to_le_bytes());
    };
    // field_dict(16)留空(L4);其余按设计顺序。
    put_pair(16, Region::default());
    put_pair(32, version);
    put_pair(48, key);
    put_pair(64, inv);
    put_pair(80, ns_stats);
    put_pair(96, zmap);
    put_pair(112, bloom);
    put_pair(128, delta);
    put_pair(144, rel);
    let crc = crc32(&out[0..HEADER_CRC_COVER]);
    out[160..164].copy_from_slice(&crc.to_le_bytes());
    out
}

/// 编码记录体(含 `u32 total_len` 前缀);WAL `Insert` 帧复用此格式。
pub(crate) fn encode_entry(entry: &EntryData) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    put_u64(&mut payload, entry.rowid.get());
    put_u64(&mut payload, entry.seqno.get());
    put_u32(&mut payload, entry.ns_id.get());

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
    payload.push(flags);

    if let Some(key) = &entry.key {
        put_bytes_u32(&mut payload, key.as_str().as_bytes());
    }
    if let Some(text) = &entry.text {
        put_bytes_u32(&mut payload, text.as_bytes());
    }
    put_bytes_u32(&mut payload, &meta::to_bytes(&entry.meta));
    put_i64(&mut payload, entry.created_at_ms);
    if let Some(expires) = entry.expires_at_ms {
        put_i64(&mut payload, expires);
    }
    if let Some(importance) = entry.importance {
        payload.extend_from_slice(&importance.to_le_bytes());
    }
    if let Some((last_access, count)) = entry.access {
        put_i64(&mut payload, last_access);
        put_u32(&mut payload, count);
    }
    if let Some((valid_from, valid_to)) = entry.valid_time {
        put_i64(&mut payload, valid_from);
        match valid_to {
            Some(valid_to) => {
                payload.push(1);
                put_i64(&mut payload, valid_to);
            }
            None => payload.push(0),
        }
    }
    if let Some(confidence) = entry.confidence {
        payload.extend_from_slice(&confidence.to_le_bytes());
    }
    if let Some(provenance) = &entry.provenance {
        put_bytes_u32(&mut payload, &meta::to_bytes(provenance));
    }

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

/// 解码单个记录体(不含长度前缀的 `body` 字节)。
///
/// # Errors
/// 字段越界、JSON 损坏或标志位矛盾时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_entry(body: &[u8]) -> Result<EntryData> {
    let mut cursor = Cursor::new(body, "msec entry");
    let rowid = RowId::new(cursor.u64()?);
    let seqno = SeqNo::new(cursor.u64()?);
    let ns_id = NsId::new(cursor.u32()?);
    let flags = cursor.u8()?;

    let key = if flags & FLAG_KEY != 0 {
        let len = cursor.u32()? as usize;
        let bytes = cursor.take(len)?;
        let text = std::str::from_utf8(bytes).map_err(|error| MnemeError::Corrupted {
            segment: None,
            reason: format!("msec: key 非 UTF-8:{error}"),
        })?;
        Some(Key::new(text))
    } else {
        None
    };
    let text = if flags & FLAG_TEXT != 0 {
        let len = cursor.u32()? as usize;
        let bytes = cursor.take(len)?;
        let value = std::str::from_utf8(bytes).map_err(|error| MnemeError::Corrupted {
            segment: None,
            reason: format!("msec: text 非 UTF-8:{error}"),
        })?;
        Some(Arc::from(value))
    } else {
        None
    };
    let meta_len = cursor.u32()? as usize;
    let meta = meta::from_bytes(cursor.take(meta_len)?)?;
    let created_at_ms = cursor.i64()?;
    let expires_at_ms = if flags & FLAG_TTL != 0 {
        Some(cursor.i64()?)
    } else {
        None
    };
    let importance = if flags & FLAG_IMPORTANCE != 0 {
        Some(read_f32(&mut cursor)?)
    } else {
        None
    };
    let access = if flags & FLAG_ACCESS != 0 {
        let last_access = cursor.i64()?;
        let count = cursor.u32()?;
        Some((last_access, count))
    } else {
        None
    };
    let valid_time = if flags & FLAG_VALID_TIME != 0 {
        let valid_from = cursor.i64()?;
        let has_to = cursor.u8()? != 0;
        let valid_to = if has_to { Some(cursor.i64()?) } else { None };
        Some((valid_from, valid_to))
    } else {
        None
    };
    let confidence = if flags & FLAG_CONFIDENCE != 0 {
        Some(read_f32(&mut cursor)?)
    } else {
        None
    };
    let provenance = if flags & FLAG_PROVENANCE != 0 {
        let len = cursor.u32()? as usize;
        Some(meta::from_bytes(cursor.take(len)?)?)
    } else {
        None
    };
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

/// 编码版本链表(定长 36 B/行)。
fn encode_version_table(rows: &[VersionRow]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rows.len() * 36);
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
    let mut out = Vec::with_capacity(rows.len() * 20);
    for row in rows {
        put_u32(&mut out, row.ns_id);
        put_u64(&mut out, row.doc_count);
        put_u64(&mut out, row.total_doc_len);
    }
    out
}

/// 校验并解析元数据段,返回各数据区视图(不复制记录体)。
///
/// # Errors
/// 魔数/版本/头部 CRC/长度不一致时返回 [`MnemeError::Corrupted`]。
pub(crate) fn parse(bytes: &[u8]) -> Result<MsecView<'_>> {
    let mut cursor = Cursor::new(bytes, "msec 头部");
    let magic = cursor.take(4)?;
    if magic != MAGIC {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "msec: 魔数不符".to_string(),
        });
    }
    let version = cursor.u16()?;
    check_version("msec", version)?;
    let header_len = cursor.u16()?;
    if header_len != HEADER_LEN {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: format!("msec: header_len={header_len} 非 {HEADER_LEN}"),
        });
    }
    if bytes.len() < HEADER_LEN as usize + 4 {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "msec: 文件短于头部".to_string(),
        });
    }
    let stored_crc = u32::from_le_bytes([bytes[160], bytes[161], bytes[162], bytes[163]]);
    if crc32(&bytes[0..HEADER_CRC_COVER]) != stored_crc {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "msec: header_crc32 不符".to_string(),
        });
    }
    // 头部偏移 8..16 为 `row_count`;随后 16..160 为 8 组 offset/len(含 field_dict)。
    let row_count = cursor.u64()?;
    let read_pair = |index: usize| -> Region {
        Region {
            offset: u64::from_le_bytes(bytes[index..index + 8].try_into().unwrap_or([0; 8])),
            len: u64::from_le_bytes(bytes[index + 8..index + 16].try_into().unwrap_or([0; 8])),
        }
    };
    let _field_dict = read_pair(16);
    let version_table = read_pair(32);
    let key_index = read_pair(48);
    let inverted = read_pair(64);
    let ns_stats = read_pair(80);
    let zmap = read_pair(96);
    let bloom = read_pair(112);
    let delta = read_pair(128);
    let relations = read_pair(144);

    let data_start = HEADER_LEN as usize;
    let data_end = bytes.len() - 4;
    let payload_crc = u32::from_le_bytes([
        bytes[data_end],
        bytes[data_end + 1],
        bytes[data_end + 2],
        bytes[data_end + 3],
    ]);
    let regions = [
        version_table,
        key_index,
        inverted,
        ns_stats,
        zmap,
        bloom,
        delta,
        relations,
    ];
    for region in regions {
        if region.len == 0 {
            continue;
        }
        let end = region
            .offset
            .checked_add(region.len)
            .ok_or_else(|| MnemeError::Corrupted {
                segment: None,
                reason: "msec: 区偏移溢出".to_string(),
            })?;
        if region.offset < data_start as u64 || end > data_end as u64 {
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: "msec: 数据区越界".to_string(),
            });
        }
    }

    Ok(MsecView {
        row_count,
        data: &bytes[data_start..data_end],
        version_table: slice_of(bytes, version_table),
        key_index: slice_of(bytes, key_index),
        ns_stats: slice_of(bytes, ns_stats),
        delta: slice_of(bytes, delta),
        relations: slice_of(bytes, relations),
        payload_crc,
        payload_crc_ok: None,
    })
}

/// 按区偏移取只读切片;空区返回空切片。
fn slice_of(bytes: &[u8], region: Region) -> &[u8] {
    if region.len == 0 {
        return &[];
    }
    let end = region.offset as usize + region.len as usize;
    &bytes[region.offset as usize..end]
}

/// 元数据段只读视图。
pub(crate) struct MsecView<'a> {
    row_count: u64,
    data: &'a [u8],
    version_table: &'a [u8],
    key_index: &'a [u8],
    ns_stats: &'a [u8],
    delta: &'a [u8],
    relations: &'a [u8],
    payload_crc: u32,
    payload_crc_ok: Option<bool>,
}

impl MsecView<'_> {
    /// 物理槽位数。
    pub(crate) const fn row_count(&self) -> u64 {
        self.row_count
    }

    /// 解析后的版本链表。
    ///
    /// # Errors
    /// 长度非 36 的倍数时返回 [`MnemeError::Corrupted`]。
    pub(crate) fn version_rows(&self) -> Result<Vec<VersionRow>> {
        if !self.version_table.len().is_multiple_of(36) {
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: "msec: version_table 长度非法".to_string(),
            });
        }
        let mut rows = Vec::with_capacity(self.version_table.len() / 36);
        let mut cursor = Cursor::new(self.version_table, "msec version_table");
        while !cursor.is_empty() {
            rows.push(VersionRow {
                rowid: cursor.u64()?,
                seqno: cursor.u64()?,
                tx_ms: cursor.i64()?,
                slot_id: cursor.u32()?,
                doc_offset: cursor.u64()?,
            });
        }
        Ok(rows)
    }

    /// 解析后的 key 索引。
    ///
    /// # Errors
    /// 变长字段越界或 key 非 UTF-8 时返回 [`MnemeError::Corrupted`]。
    pub(crate) fn key_rows(&self) -> Result<Vec<KeyIndexRow>> {
        let mut rows = Vec::new();
        let mut cursor = Cursor::new(self.key_index, "msec key_index");
        while !cursor.is_empty() {
            let ns_id = cursor.u32()?;
            let len = cursor.u32()? as usize;
            let key =
                std::str::from_utf8(cursor.take(len)?).map_err(|error| MnemeError::Corrupted {
                    segment: None,
                    reason: format!("msec: key 非 UTF-8:{error}"),
                })?;
            rows.push(KeyIndexRow {
                ns_id,
                key: Arc::from(key),
                rowid: cursor.u64()?,
                slot_id: cursor.u32()?,
                seqno: cursor.u64()?,
                doc_offset: cursor.u64()?,
            });
        }
        Ok(rows)
    }

    /// 解析后的命名空间统计。
    ///
    /// # Errors
    /// 长度非 20 的倍数时返回 [`MnemeError::Corrupted`]。
    pub(crate) fn ns_stat_rows(&self) -> Result<Vec<NsStatRow>> {
        if !self.ns_stats.len().is_multiple_of(20) {
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: "msec: ns_stats 长度非法".to_string(),
            });
        }
        let mut rows = Vec::with_capacity(self.ns_stats.len() / 20);
        let mut cursor = Cursor::new(self.ns_stats, "msec ns_stats");
        while !cursor.is_empty() {
            rows.push(NsStatRow {
                ns_id: cursor.u32()?,
                doc_count: cursor.u64()?,
                total_doc_len: cursor.u64()?,
            });
        }
        Ok(rows)
    }

    /// delta 区原始字节(可能为空)。
    pub(crate) const fn delta_bytes(&self) -> &[u8] {
        self.delta
    }

    /// relations 区原始字节(可能为空)。
    pub(crate) const fn relations_bytes(&self) -> &[u8] {
        self.relations
    }

    /// 读取某个 `doc_offset` 处的记录体;墓碑偏移返回 `None`。
    ///
    /// # Errors
    /// 偏移越界或记录体解码失败时返回 [`MnemeError::Corrupted`]。
    pub(crate) fn read_entry(&self, doc_offset: u64) -> Result<Option<EntryData>> {
        if doc_offset == TOMBSTONE_DOC_OFFSET {
            return Ok(None);
        }
        let start = usize::try_from(doc_offset).map_err(|_| MnemeError::Corrupted {
            segment: None,
            reason: "msec: doc_offset 超出 usize".to_string(),
        })?;
        if start + 4 > self.data.len() {
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: "msec: doc_offset 越界".to_string(),
            });
        }
        let total_len = u32::from_le_bytes([
            self.data[start],
            self.data[start + 1],
            self.data[start + 2],
            self.data[start + 3],
        ]) as usize;
        let body_start = start + 4;
        let body_end = body_start + total_len;
        if body_end > self.data.len() {
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: "msec: 记录体长度越界".to_string(),
            });
        }
        Ok(Some(decode_entry(&self.data[body_start..body_end])?))
    }

    /// 校验数据区 payload CRC;结果被缓存。
    ///
    /// # Errors
    /// CRC 不符时返回 [`MnemeError::Corrupted`]。
    pub(crate) fn verify_payload(&mut self) -> Result<()> {
        let ok = *self
            .payload_crc_ok
            .get_or_insert_with(|| crc32(self.data) == self.payload_crc);
        if ok {
            Ok(())
        } else {
            Err(MnemeError::Corrupted {
                segment: None,
                reason: "msec: payload_crc32 不符".to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::meta::json;

    fn body(rowid: u64, seqno: u64, ns: u32, key: Option<&str>) -> EntryData {
        EntryData {
            rowid: RowId::new(rowid),
            seqno: SeqNo::new(seqno),
            ns_id: NsId::new(ns),
            key: key.map(Key::new),
            text: Some(Arc::from("hello world")),
            meta: json!({"kind": "note"}),
            created_at_ms: 1_700_000_000_000,
            expires_at_ms: Some(1_800_000_000_000),
            importance: Some(0.7),
            access: Some((1_700_000_000_100, 3)),
            valid_time: Some((1_000, Some(2_000))),
            confidence: Some(0.9),
            provenance: Some(json!({"source": "user"})),
        }
    }

    fn sample_slots() -> Vec<SlotMeta> {
        vec![
            SlotMeta {
                rowid: RowId::new(0),
                seqno: SeqNo::new(1),
                tx_ms: 10,
                body: Some(body(0, 1, 1, Some("alpha"))),
            },
            SlotMeta {
                rowid: RowId::new(0),
                seqno: SeqNo::new(2),
                tx_ms: 20,
                body: None,
            },
            SlotMeta {
                rowid: RowId::new(1),
                seqno: SeqNo::new(3),
                tx_ms: 30,
                body: Some(body(1, 3, 2, None)),
            },
        ]
    }

    fn encode_sample() -> Vec<u8> {
        let slots = sample_slots();
        let ns_stats = [NsStatRow {
            ns_id: 1,
            doc_count: 1,
            total_doc_len: 11,
        }];
        encode(&MsecInput {
            slots: &slots,
            ns_stats: &ns_stats,
            delta: &[],
            relations: &[],
        })
        .expect("encode")
    }

    /// 记录体字段往返一致。
    #[test]
    fn msec_entry_roundtrip() {
        let entry = body(7, 9, 3, Some("k"));
        let bytes = encode_entry(&entry).expect("encode");
        assert_eq!(entry_from_prefix(&bytes).expect("decode"), entry);
    }

    /// 段级往返:版本链、key 索引、墓碑 `doc_offset` 与统计一致。
    #[test]
    fn msec_segment_roundtrip() {
        let slots = sample_slots();
        let bytes = encode_sample();
        let mut view = parse(&bytes).expect("parse");
        assert_eq!(view.row_count(), 3);
        let versions = view.version_rows().expect("versions");
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].slot_id, 0);
        assert_eq!(versions[1].slot_id, 1);
        assert_eq!(versions[1].doc_offset, TOMBSTONE_DOC_OFFSET);
        assert_eq!(versions[2].slot_id, 2);

        let keys = view.key_rows().expect("keys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key.as_ref(), "alpha");
        assert_eq!(keys[0].ns_id, 1);

        let stats = view.ns_stat_rows().expect("stats");
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].ns_id, 1);

        let first = view.read_entry(versions[0].doc_offset).expect("read");
        assert_eq!(first.as_ref(), slots[0].body.as_ref());
        assert!(
            view.read_entry(TOMBSTONE_DOC_OFFSET)
                .expect("tomb")
                .is_none()
        );
        view.verify_payload().expect("payload crc");
    }

    /// 头部损坏必须被检出。
    #[test]
    fn msec_detects_header_corruption() {
        let mut bytes = encode_sample();
        bytes[8] ^= 0xFF;
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// 魔数不符 → `Corrupted`。
    #[test]
    fn msec_rejects_bad_magic() {
        let mut bytes = encode_sample();
        bytes[0] = b'X';
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }
}
