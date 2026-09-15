//! MANIFEST 定长头部(72 B)的校验与解析。
//!
//! 头部包含魔数、格式版本、头部 CRC 与各变长表计数,字段偏移见模块文档。

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::persist::vsec::metric_from_u8;
use crate::persist::{FORMAT_VERSION, check_version};

/// MANIFEST 魔数。
pub(crate) const MAGIC: [u8; 4] = *b"MNF1";
/// 定长头部长度(字节)。
pub(crate) const HEADER_LEN: u16 = 72;

/// `header[17]` 的 stopwords 标志:显式关闭。
pub(super) const STOPWORDS_DISABLED: u8 = 1;
/// `header[17]` 的 stopwords 标志:显式开启。
pub(super) const STOPWORDS_ENABLED: u8 = 2;

/// 计算头部 CRC(覆盖除 [8,12) 外的全部头部字节)。
pub(super) fn header_crc(header: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&header[0..8]);
    hasher.update(&header[12..]);
    hasher.finalize()
}

/// MANIFEST 定长头部字段与变长区行数。
pub(super) struct ManifestHeader {
    pub(super) dimension: u32,
    pub(super) metric: Metric,
    pub(super) stopwords: bool,
    pub(super) next_rel_kind: u16,
    pub(super) manifest_version: u64,
    pub(super) watermark_seqno: u64,
    pub(super) next_rowid: u64,
    pub(super) next_segment_id: u32,
    pub(super) next_ns_id: u32,
    pub(super) active_count: usize,
    pub(super) ns_count: usize,
    pub(super) rel_kind_count: usize,
}

/// 校验并解析 MANIFEST 定长头部。
pub(super) fn parse_header(bytes: &[u8]) -> Result<ManifestHeader> {
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

/// 读取小端 `u64`。
fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap_or([0; 8]))
}

/// 读取小端 `u32`。
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]))
}
