//! MANIFEST 解码与交叉校验:头部 → 三张变长表 → 水位校验,任何不符按损坏拒绝。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::persist::{Cursor, crc32};

use super::header::{HEADER_LEN, parse_header};
use super::model::{Manifest, NsEntry, RelKindEntry, SegmentEntry};

/// 单个段条目的定长字节数(见 [`parse_segments`] 字段顺序)。
const SEGMENT_ENTRY_BYTES: usize = 55;

/// 校验并解析 MANIFEST。
///
/// # Errors
/// 魔数/版本/头部 CRC/payload CRC/长度不符时返回 [`MnemeError::Corrupted`]。
pub(crate) fn parse(bytes: &[u8]) -> Result<Manifest> {
    let header = parse_header(bytes)?;
    let body = &bytes[HEADER_LEN as usize..bytes.len() - 4];
    validate_payload_crc(body, bytes)?;
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
    validate_next_rel_kind(header.next_rel_kind, &rel_kinds)?;
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

/// 校验 payload 区 CRC 与文件末 4 字节声明一致。
fn validate_payload_crc(body: &[u8], bytes: &[u8]) -> Result<()> {
    let stored_payload_crc =
        u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().unwrap_or([0; 4]));
    if crc32(body) != stored_payload_crc {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "manifest: payload_crc32 不符".to_string(),
        });
    }
    Ok(())
}

/// 校验关系类型水位:必须落在自定义区间且不落后于已登记最大编号
/// (否则下一次分配会复用内置编号或撞号,FC-MODEL-POST-008)。
fn validate_next_rel_kind(next_rel_kind: u16, rel_kinds: &[RelKindEntry]) -> Result<()> {
    if next_rel_kind < crate::core::options::RelationKind::FIRST_CUSTOM {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: format!(
                "manifest: next_rel_kind {} 低于自定义起点 {}",
                next_rel_kind,
                crate::core::options::RelationKind::FIRST_CUSTOM
            ),
        });
    }
    // 水位不得落后于已登记条目(否则下一次分配会撞号)。
    if let Some(max_kind) = rel_kinds.iter().map(|entry| entry.kind).max()
        && next_rel_kind <= max_kind
    {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "manifest: next_rel_kind 不大于已登记最大编号".to_string(),
        });
    }
    Ok(())
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
///
/// 编号必须 ≥ 16(内置区间保留)、名称非空且库内唯一;同名不同号或同号不同名
/// → `Corrupted`(FC-MODEL-POST-008),绝不静默建立歧义注册表。
fn parse_rel_kinds(cursor: &mut Cursor<'_>, count: usize) -> Result<Vec<RelKindEntry>> {
    let mut rel_kinds = Vec::with_capacity(count.min(cursor.remaining() / 6));
    let mut names: std::collections::HashMap<Arc<str>, u16> = std::collections::HashMap::new();
    let mut kinds: std::collections::HashMap<u16, Arc<str>> = std::collections::HashMap::new();
    let corrupt = |reason: &str| MnemeError::Corrupted {
        segment: None,
        reason: format!("manifest: rel_kind {reason}"),
    };
    for _ in 0..count {
        let kind = cursor.u16()?;
        let len = cursor.u32()? as usize;
        let name = read_utf8(cursor, len, "name")?;
        if kind < crate::core::options::RelationKind::FIRST_CUSTOM || name.is_empty() {
            return Err(corrupt("编号低于自定义起点或名称为空"));
        }
        if let Some(&existing) = names.get(&name)
            && existing != kind
        {
            return Err(corrupt("同名不同编号"));
        }
        if let Some(existing) = kinds.get(&kind)
            && existing.as_ref() != name.as_ref()
        {
            return Err(corrupt("同编号不同名"));
        }
        names.insert(Arc::clone(&name), kind);
        kinds.insert(kind, Arc::clone(&name));
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
