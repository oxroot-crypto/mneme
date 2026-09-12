//! msec 段头解析与只读视图(`msec/decode.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::persist::{Cursor, FORMAT_VERSION, check_version, crc32};

use super::entry::decode_entry;
use super::{
    EntryData, HEADER_CRC_COVER, HEADER_LEN, KeyIndexRow, MAGIC, NS_STAT_ROW_BYTES, NsStatRow,
    Region, Regions, TOMBSTONE_DOC_OFFSET, VERSION_ROW_BYTES, VersionRow,
};

/// 头部解析结果:行数、9 个数据区与 payload CRC。
struct HeaderLayout {
    row_count: u64,
    regions: Regions,
    payload_crc: u32,
}

/// 校验并解析元数据段,返回各数据区视图(不复制记录体)。
///
/// # Errors
/// 魔数/版本/头部 CRC/长度不一致时返回 [`MnemeError::Corrupted`]。
pub(crate) fn parse(bytes: &[u8]) -> Result<MsecView<'_>> {
    let header = parse_header(bytes)?;
    let data_start = HEADER_LEN as usize;
    let data_end = bytes.len() - 4;
    validate_regions(&header.regions, data_start, data_end)?;
    Ok(MsecView {
        row_count: header.row_count,
        data: &bytes[data_start..data_end],
        field_dict: slice_of(bytes, header.regions.field_dict),
        version_table: slice_of(bytes, header.regions.version),
        key_index: slice_of(bytes, header.regions.key),
        inverted: slice_of(bytes, header.regions.inv),
        ns_stats: slice_of(bytes, header.regions.ns_stats),
        zmap: slice_of(bytes, header.regions.zmap),
        bloom: slice_of(bytes, header.regions.bloom),
        delta: slice_of(bytes, header.regions.delta),
        relations: slice_of(bytes, header.regions.rel),
        payload_crc: header.payload_crc,
        payload_crc_ok: None,
    })
}

/// 校验并解析定长头部。
fn parse_header(bytes: &[u8]) -> Result<HeaderLayout> {
    let mut cursor = Cursor::new(bytes, "msec 头部");
    if cursor.take(4)? != MAGIC {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "msec: 魔数不符".to_string(),
        });
    }
    let version = cursor.u16()?;
    check_version("msec", version, FORMAT_VERSION)?;
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
    // 头部偏移 8..16 为 `row_count`;随后 16..160 为 9 组 offset/len(含 field_dict)。
    let row_count = cursor.u64()?;
    Ok(HeaderLayout {
        row_count,
        regions: Regions {
            field_dict: read_pair(bytes, 16),
            version: read_pair(bytes, 32),
            key: read_pair(bytes, 48),
            inv: read_pair(bytes, 64),
            ns_stats: read_pair(bytes, 80),
            zmap: read_pair(bytes, 96),
            bloom: read_pair(bytes, 112),
            delta: read_pair(bytes, 128),
            rel: read_pair(bytes, 144),
        },
        payload_crc: read_u32_at(bytes, bytes.len() - 4),
    })
}

/// 读取头部第 `index` 字节起的 `offset/len` 对。
fn read_pair(bytes: &[u8], index: usize) -> Region {
    Region {
        offset: u64::from_le_bytes(bytes[index..index + 8].try_into().unwrap_or([0; 8])),
        len: u64::from_le_bytes(bytes[index + 8..index + 16].try_into().unwrap_or([0; 8])),
    }
}

/// 读取 4 字节小端 `u32`。
fn read_u32_at(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[index..index + 4].try_into().unwrap_or([0; 4]))
}

/// 校验各非空数据区的偏移/长度落在数据区内。
fn validate_regions(regions: &Regions, data_start: usize, data_end: usize) -> Result<()> {
    let all = [
        regions.field_dict,
        regions.version,
        regions.key,
        regions.inv,
        regions.ns_stats,
        regions.zmap,
        regions.bloom,
        regions.delta,
        regions.rel,
    ];
    for region in all {
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
    Ok(())
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
    field_dict: &'a [u8],
    version_table: &'a [u8],
    key_index: &'a [u8],
    inverted: &'a [u8],
    ns_stats: &'a [u8],
    zmap: &'a [u8],
    bloom: &'a [u8],
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
        if !self.version_table.len().is_multiple_of(VERSION_ROW_BYTES) {
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: "msec: version_table 长度非法".to_string(),
            });
        }
        let mut rows = Vec::with_capacity(self.version_table.len() / VERSION_ROW_BYTES);
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
        if !self.ns_stats.len().is_multiple_of(NS_STAT_ROW_BYTES) {
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: "msec: ns_stats 长度非法".to_string(),
            });
        }
        let mut rows = Vec::with_capacity(self.ns_stats.len() / NS_STAT_ROW_BYTES);
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

    /// relations 区原始字节(空表也带 `EDG1` 头)。
    pub(crate) const fn relations_bytes(&self) -> &[u8] {
        self.relations
    }

    /// delta 区原始字节(空区 = 本段无跨段变更)。
    pub(crate) const fn delta_bytes(&self) -> &[u8] {
        self.delta
    }

    /// 字段字典区原始字节(恒非空)。
    pub(crate) const fn field_dict_bytes(&self) -> &[u8] {
        self.field_dict
    }

    /// zone map 区原始字节(含区尾 `ttl_map`)。
    pub(crate) const fn zmap_bytes(&self) -> &[u8] {
        self.zmap
    }

    /// bloom 区原始字节(`key` 字段)。
    pub(crate) const fn bloom_bytes(&self) -> &[u8] {
        self.bloom
    }

    /// 倒排区原始字节(空表也带版头)。
    pub(crate) const fn inverted_bytes(&self) -> &[u8] {
        self.inverted
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
        // 全用 checked 加法:损坏的 doc_offset 不得让下标回绕而越界切片(FC-GLOBAL-ERR-001)。
        let body_start = start.checked_add(4).ok_or_else(|| MnemeError::Corrupted {
            segment: None,
            reason: "msec: doc_offset 溢出".to_string(),
        })?;
        if body_start > self.data.len() {
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: "msec: doc_offset 越界".to_string(),
            });
        }
        let total_len = read_u32_at(self.data, start) as usize;
        let body_end = body_start
            .checked_add(total_len)
            .ok_or_else(|| MnemeError::Corrupted {
                segment: None,
                reason: "msec: 记录体长度溢出".to_string(),
            })?;
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
