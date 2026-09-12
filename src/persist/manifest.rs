//! MANIFEST 编解码(设计 04 §2.4、§6)。
//!
//! MANIFEST 是**命名空间路径与库级维度/度量的唯一事实来源**,也是活跃段集合与
//! ID 水位的持久载体;更新走 "write-once 版本文件 + `current` 指针"(§6)。
//!
//! ```text
//! 0   magic "MNF1" | 4 u16 ver | 6 u16 header_len | 8 u32 header_crc32
//! 12  u32 dimension | 16 u8 metric | 17 u8 stopwords | 18 reserved[4] | 22 u16 next_rel_kind
//! 24  u64 manifest_version | 32 u64 watermark_seqno | 40 u64 next_rowid
//! 48  u32 next_segment_id | 52 u32 next_ns_id | 56 u32 active_count
//! 60  u32 ns_count | 64 u32 rel_kind_count | 68..72 pad
//! 变长区: NsEntry×ns_count → RelKindEntry×rel_kind_count → SegmentEntry×active_count
//! 尾部:   u32 payload_crc32(覆盖变长区)
//! ```
//! `header_crc32` 覆盖头部除自身(偏移 8..12)外的全部字节。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::persist::vsec::{metric_from_u8, metric_to_u8};
use crate::persist::{
    Cursor, FORMAT_VERSION, check_version, crc32, put_bytes_u32, put_i64, put_u16, put_u32, put_u64,
};

/// MANIFEST 魔数。
pub(crate) const MAGIC: [u8; 4] = *b"MNF1";
/// 定长头部长度(字节)。
pub(crate) const HEADER_LEN: u16 = 72;
/// 单个段条目的定长字节数(见 [`parse_segments`] 字段顺序)。
const SEGMENT_ENTRY_BYTES: usize = 55;

/// `header[17]` 的 stopwords 标志:显式关闭。
const STOPWORDS_DISABLED: u8 = 1;
/// `header[17]` 的 stopwords 标志:显式开启。
const STOPWORDS_ENABLED: u8 = 2;

/// 命名空间注册项(`path ↔ NsId`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NsEntry {
    /// 命名空间编号。
    pub(crate) ns_id: u32,
    /// 命名空间路径(UTF-8)。
    pub(crate) path: Arc<str>,
}

/// 关系类型注册项(`kind ↔ name`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelKindEntry {
    /// 关系类型编号。
    pub(crate) kind: u16,
    /// 类型名(UTF-8)。
    pub(crate) name: Arc<str>,
}

/// 活跃段条目。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SegmentEntry {
    /// 段编号。
    pub(crate) segment_id: u32,
    /// 段文件格式版本(审计用;打开时以段文件自身头部的版本为权威,不读本字段)。
    pub(crate) format_version: u16,
    /// 行数。
    pub(crate) row_count: u64,
    /// 最小写序号。
    pub(crate) min_seqno: u64,
    /// 最大写序号。
    pub(crate) max_seqno: u64,
    /// 创建时刻(Unix 毫秒)。
    pub(crate) created_ms: i64,
    /// vsec 文件 CRC。
    pub(crate) vsec_crc: u32,
    /// msec 文件 CRC。
    pub(crate) msec_crc: u32,
    /// hidx 文件 CRC(无 hidx 时为 0)。
    pub(crate) hidx_crc: u32,
    /// HNSW 入口槽位(L3 起;L2 为 0)。
    pub(crate) entry_slot: u32,
    /// HNSW 入口层级(L3 起;L2 为 0)。
    pub(crate) entry_level: u8,
}

/// MANIFEST 全部字段。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Manifest {
    /// 建库维度(建库即锁定)。
    pub(crate) dimension: u32,
    /// 距离度量(建库即锁定)。
    pub(crate) metric: Metric,
    /// 文本分词是否启用停用词(建库即锁定;查询与索引必须同口径)。
    pub(crate) stopwords: bool,
    /// 自定义关系类型编号分配水位(永不复用)。
    pub(crate) next_rel_kind: u16,
    /// MANIFEST 版本号。
    pub(crate) manifest_version: u64,
    /// 已物化水位:回放只处理 `seqno > watermark_seqno` 的帧。
    pub(crate) watermark_seqno: u64,
    /// 全局 RowId 分配水位(永不复用)。
    pub(crate) next_rowid: u64,
    /// 段编号分配水位。
    pub(crate) next_segment_id: u32,
    /// 命名空间编号分配水位(永不复用)。
    pub(crate) next_ns_id: u32,
    /// 命名空间注册表。
    pub(crate) namespaces: Vec<NsEntry>,
    /// 关系类型注册表。
    pub(crate) rel_kinds: Vec<RelKindEntry>,
    /// 活跃段集合。
    pub(crate) segments: Vec<SegmentEntry>,
}

/// 编码一个 MANIFEST。
///
/// # Errors
/// 条目数或文件长度溢出 `u32` 时返回 [`MnemeError::TooLarge`]。
pub(crate) fn encode(manifest: &Manifest) -> Result<Vec<u8>> {
    let body = encode_body(manifest);
    let header = encode_manifest_header(manifest)?;
    let mut out = header.to_vec();
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_le_bytes());
    Ok(out)
}

/// 编码命名空间 / 关系类型 / 活跃段三个变长表。
fn encode_body(manifest: &Manifest) -> Vec<u8> {
    let mut body = Vec::new();
    for ns in &manifest.namespaces {
        put_u32(&mut body, ns.ns_id);
        put_bytes_u32(&mut body, ns.path.as_bytes());
    }
    for rel in &manifest.rel_kinds {
        put_u16(&mut body, rel.kind);
        put_bytes_u32(&mut body, rel.name.as_bytes());
    }
    for seg in &manifest.segments {
        put_u32(&mut body, seg.segment_id);
        put_u16(&mut body, seg.format_version);
        put_u64(&mut body, seg.row_count);
        put_u64(&mut body, seg.min_seqno);
        put_u64(&mut body, seg.max_seqno);
        put_i64(&mut body, seg.created_ms);
        put_u32(&mut body, seg.vsec_crc);
        put_u32(&mut body, seg.msec_crc);
        put_u32(&mut body, seg.hidx_crc);
        put_u32(&mut body, seg.entry_slot);
        body.push(seg.entry_level);
    }
    body
}

/// 编码定长头部(含头部 CRC)。
fn encode_manifest_header(manifest: &Manifest) -> Result<[u8; HEADER_LEN as usize]> {
    let mut header = [0_u8; HEADER_LEN as usize];
    header[0..4].copy_from_slice(&MAGIC);
    header[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    header[6..8].copy_from_slice(&HEADER_LEN.to_le_bytes());
    header[12..16].copy_from_slice(&manifest.dimension.to_le_bytes());
    header[16] = metric_to_u8(manifest.metric);
    header[17] = if manifest.stopwords {
        STOPWORDS_ENABLED
    } else {
        STOPWORDS_DISABLED
    };
    header[22..24].copy_from_slice(&manifest.next_rel_kind.to_le_bytes());
    header[24..32].copy_from_slice(&manifest.manifest_version.to_le_bytes());
    header[32..40].copy_from_slice(&manifest.watermark_seqno.to_le_bytes());
    header[40..48].copy_from_slice(&manifest.next_rowid.to_le_bytes());
    header[48..52].copy_from_slice(&manifest.next_segment_id.to_le_bytes());
    header[52..56].copy_from_slice(&manifest.next_ns_id.to_le_bytes());
    header[56..60]
        .copy_from_slice(&count_u32(manifest.segments.len(), "active_count")?.to_le_bytes());
    header[60..64]
        .copy_from_slice(&count_u32(manifest.namespaces.len(), "ns_count")?.to_le_bytes());
    header[64..68]
        .copy_from_slice(&count_u32(manifest.rel_kinds.len(), "rel_kind_count")?.to_le_bytes());
    let crc = header_crc(&header);
    header[8..12].copy_from_slice(&crc.to_le_bytes());
    Ok(header)
}

/// 计算头部 CRC(覆盖除 [8,12) 外的全部头部字节)。
fn header_crc(header: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&header[0..8]);
    hasher.update(&header[12..]);
    hasher.finalize()
}

/// 把计数转换为 `u32`,溢出返回 [`MnemeError::TooLarge`]。
fn count_u32(value: usize, field: &'static str) -> Result<u32> {
    u32::try_from(value).map_err(|_| MnemeError::TooLarge {
        field,
        limit: u32::MAX as usize,
        got: value,
    })
}

/// 校验并解析 MANIFEST。
///
/// # Errors
/// 魔数/版本/头部 CRC/payload CRC/长度不符时返回 [`MnemeError::Corrupted`]。
pub(crate) fn parse(bytes: &[u8]) -> Result<Manifest> {
    let header = parse_header(bytes)?;
    let body = &bytes[HEADER_LEN as usize..bytes.len() - 4];
    let stored_payload_crc =
        u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().unwrap_or([0; 4]));
    if crc32(body) != stored_payload_crc {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "manifest: payload_crc32 不符".to_string(),
        });
    }
    let mut cursor = Cursor::new(body, "manifest 变长区");
    let namespaces = parse_namespaces(&mut cursor, header.ns_count)?;
    let rel_kinds = parse_rel_kinds(&mut cursor, header.rel_kind_count)?;
    let segments = parse_segments(&mut cursor, header.active_count)?;
    if !cursor.is_empty() {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "manifest: 变长区尾部有残留字节".to_string(),
        });
    }
    Ok(Manifest {
        dimension: header.dimension,
        metric: header.metric,
        stopwords: header.stopwords,
        next_rel_kind: header.next_rel_kind,
        manifest_version: header.manifest_version,
        watermark_seqno: header.watermark_seqno,
        next_rowid: header.next_rowid,
        next_segment_id: header.next_segment_id,
        next_ns_id: header.next_ns_id,
        namespaces,
        rel_kinds,
        segments,
    })
}

/// MANIFEST 定长头部字段与变长区行数。
struct ManifestHeader {
    dimension: u32,
    metric: Metric,
    stopwords: bool,
    next_rel_kind: u16,
    manifest_version: u64,
    watermark_seqno: u64,
    next_rowid: u64,
    next_segment_id: u32,
    next_ns_id: u32,
    active_count: usize,
    ns_count: usize,
    rel_kind_count: usize,
}

/// 校验并解析 MANIFEST 定长头部。
fn parse_header(bytes: &[u8]) -> Result<ManifestHeader> {
    validate_header(bytes)?;
    Ok(ManifestHeader {
        dimension: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        metric: metric_from_u8(bytes[16])?,
        stopwords: decode_stopwords(bytes[17])?,
        next_rel_kind: u16::from_le_bytes([bytes[22], bytes[23]]),
        manifest_version: read_u64(bytes, 24),
        watermark_seqno: read_u64(bytes, 32),
        next_rowid: read_u64(bytes, 40),
        next_segment_id: read_u32(bytes, 48),
        next_ns_id: read_u32(bytes, 52),
        active_count: read_u32(bytes, 56) as usize,
        ns_count: read_u32(bytes, 60) as usize,
        rel_kind_count: read_u32(bytes, 64) as usize,
    })
}

/// 校验头部长度、魔数、版本、`header_len` 与头部 CRC。
fn validate_header(bytes: &[u8]) -> Result<()> {
    if bytes.len() < HEADER_LEN as usize + 4 {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "manifest: 文件短于头部".to_string(),
        });
    }
    if bytes[0..4] != MAGIC {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "manifest: 魔数不符".to_string(),
        });
    }
    check_version(
        "manifest",
        u16::from_le_bytes([bytes[4], bytes[5]]),
        FORMAT_VERSION,
    )?;
    let header_len = u16::from_le_bytes([bytes[6], bytes[7]]);
    if header_len != HEADER_LEN {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: format!("manifest: header_len={header_len} 非 {HEADER_LEN}"),
        });
    }
    let stored_header_crc = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    if header_crc(&bytes[..HEADER_LEN as usize]) != stored_header_crc {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "manifest: header_crc32 不符".to_string(),
        });
    }
    Ok(())
}

/// 解析 `stopwords` 标志;仅接受显式开/关,其余值按损坏拒绝。
fn decode_stopwords(flag: u8) -> Result<bool> {
    match flag {
        STOPWORDS_ENABLED => Ok(true),
        STOPWORDS_DISABLED => Ok(false),
        _ => Err(MnemeError::Corrupted {
            segment: None,
            reason: "manifest: stopwords 标志非法".to_string(),
        }),
    }
}

/// 解析命名空间条目列表(按剩余字节数设预分配上界,抵御损坏计数)。
fn parse_namespaces(cursor: &mut Cursor<'_>, count: usize) -> Result<Vec<NsEntry>> {
    let mut namespaces = Vec::with_capacity(count.min(cursor.remaining() / 8));
    for _ in 0..count {
        let ns_id = cursor.u32()?;
        let len = cursor.u32()? as usize;
        let path = read_utf8(cursor, len, "path")?;
        namespaces.push(NsEntry { ns_id, path });
    }
    Ok(namespaces)
}

/// 解析关系类型条目列表(按剩余字节数设预分配上界,抵御损坏计数)。
fn parse_rel_kinds(cursor: &mut Cursor<'_>, count: usize) -> Result<Vec<RelKindEntry>> {
    let mut rel_kinds = Vec::with_capacity(count.min(cursor.remaining() / 6));
    for _ in 0..count {
        let kind = cursor.u16()?;
        let len = cursor.u32()? as usize;
        let name = read_utf8(cursor, len, "name")?;
        rel_kinds.push(RelKindEntry { kind, name });
    }
    Ok(rel_kinds)
}

/// 解析段条目列表(每条定长 55 B;按剩余字节数设预分配上界,抵御损坏计数)。
fn parse_segments(cursor: &mut Cursor<'_>, count: usize) -> Result<Vec<SegmentEntry>> {
    let mut segments = Vec::with_capacity(count.min(cursor.remaining() / SEGMENT_ENTRY_BYTES));
    for _ in 0..count {
        segments.push(SegmentEntry {
            segment_id: cursor.u32()?,
            format_version: cursor.u16()?,
            row_count: cursor.u64()?,
            min_seqno: cursor.u64()?,
            max_seqno: cursor.u64()?,
            created_ms: cursor.i64()?,
            vsec_crc: cursor.u32()?,
            msec_crc: cursor.u32()?,
            hidx_crc: cursor.u32()?,
            entry_slot: cursor.u32()?,
            entry_level: cursor.u8()?,
        });
    }
    Ok(segments)
}

/// 读取 UTF-8 字符串。
fn read_utf8(cursor: &mut Cursor<'_>, len: usize, field: &str) -> Result<Arc<str>> {
    let bytes = cursor.take(len)?;
    let value = std::str::from_utf8(bytes).map_err(|error| MnemeError::Corrupted {
        segment: None,
        reason: format!("manifest: {field} 非 UTF-8:{error}"),
    })?;
    Ok(Arc::from(value))
}

/// 读取小端 `u64`。
fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap_or([0; 8]))
}

/// 读取小端 `u32`。
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            dimension: 768,
            metric: Metric::Cosine,
            stopwords: true,
            next_rel_kind: 20,
            manifest_version: 42,
            watermark_seqno: 105,
            next_rowid: 7,
            next_segment_id: 3,
            next_ns_id: 2,
            namespaces: vec![NsEntry {
                ns_id: 1,
                path: Arc::from("project/session"),
            }],
            rel_kinds: vec![RelKindEntry {
                kind: 16,
                name: Arc::from("custom"),
            }],
            segments: vec![SegmentEntry {
                segment_id: 7,
                format_version: FORMAT_VERSION,
                row_count: 100,
                min_seqno: 1,
                max_seqno: 100,
                created_ms: 1_700_000_000_000,
                vsec_crc: 0xAA,
                msec_crc: 0xBB,
                hidx_crc: 0,
                entry_slot: 0,
                entry_level: 0,
            }],
        }
    }

    /// 全字段往返一致。
    #[test]
    fn manifest_roundtrip() {
        let manifest = sample();
        let bytes = encode(&manifest).expect("encode");
        assert_eq!(parse(&bytes).expect("parse"), manifest);
    }

    /// FC-PERSIST-POST-009(stopwords 显式开/关往返;非法值拒绝)
    #[test]
    fn stopwords_flag_roundtrip() {
        for stopwords in [true, false] {
            let mut manifest = sample();
            manifest.stopwords = stopwords;
            let bytes = encode(&manifest).expect("encode");
            let decoded = parse(&bytes).expect("parse");
            assert_eq!(decoded.stopwords, stopwords);
        }

        // 未知值(含 0)→ Corrupted。
        for flag in [0_u8, 9] {
            let mut bytes = encode(&sample()).expect("encode");
            bytes[17] = flag;
            let crc = header_crc(&bytes[..HEADER_LEN as usize]);
            bytes[8..12].copy_from_slice(&crc.to_le_bytes());
            assert!(
                matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })),
                "stopwords 标志 {flag} 必须拒绝"
            );
        }
    }

    /// 头部 CRC 损坏被检出。
    #[test]
    fn manifest_detects_header_corruption() {
        let mut bytes = encode(&sample()).expect("encode");
        bytes[12] ^= 0xFF;
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// 变长区 CRC 损坏被检出。
    #[test]
    fn manifest_detects_payload_corruption() {
        let mut bytes = encode(&sample()).expect("encode");
        let last = bytes.len() - 5;
        bytes[last] ^= 0xFF;
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// 更高/更低版本 → `UnsupportedVersion`。
    #[test]
    fn manifest_rejects_version_mismatch() {
        for version in [0x0100_u16, FORMAT_VERSION - 1] {
            let manifest = sample();
            let mut bytes = encode(&manifest).expect("encode");
            bytes[4..6].copy_from_slice(&version.to_le_bytes());
            let crc = header_crc(&bytes[..HEADER_LEN as usize]);
            bytes[8..12].copy_from_slice(&crc.to_le_bytes());
            assert!(matches!(
                parse(&bytes),
                Err(MnemeError::UnsupportedVersion { .. })
            ));
        }
    }
}
