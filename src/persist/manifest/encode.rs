//! MANIFEST 编码:定长头部 + 命名空间/关系类型/活跃段三张变长表 + payload CRC。

use crate::core::error::{MnemeError, Result};
use crate::persist::vsec::metric_to_u8;
use crate::persist::{FORMAT_VERSION, crc32, put_bytes_u32, put_i64, put_u16, put_u32, put_u64};

use super::header::{HEADER_LEN, MAGIC, STOPWORDS_DISABLED, STOPWORDS_ENABLED, header_crc};
use super::model::Manifest;

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

/// 把计数转换为 `u32`,溢出返回 [`MnemeError::TooLarge`]。
fn count_u32(value: usize, field: &'static str) -> Result<u32> {
    u32::try_from(value).map_err(|_| MnemeError::TooLarge {
        field,
        limit: u32::MAX as usize,
        got: value,
    })
}
