//! msec zone map 区同区尾 `ttl_map` 编解码(块级 TTL 剪枝)。

use std::sync::Arc;

use crate::core::error::Result;
use crate::memory::analysis::ZoneIndex;
use crate::persist::{Cursor, put_u32, put_u64};

use super::common::corrupted;
use super::field_dict::{FieldDef, FieldKind};

/// 编码 zone map 区(仅数值/时间字段;缺失块补空统计)。
pub(crate) fn encode_zmap(
    zones: &ZoneIndex,
    fields: &[(Arc<str>, FieldKind)],
    block_count: usize,
) -> Vec<u8> {
    let mut out = Vec::new();
    let indexed = fields
        .iter()
        .filter(|(_, kind)| *kind != FieldKind::Str)
        .count();
    put_u32(&mut out, indexed as u32);
    put_u64(&mut out, block_count as u64);
    for (field_id, (name, kind)) in fields.iter().enumerate() {
        if *kind == FieldKind::Str {
            continue;
        }
        put_u32(&mut out, field_id as u32);
        for block in 0..block_count {
            let stat = zones.block_stat(name, block).unwrap_or_default();
            out.extend_from_slice(&stat.min.to_le_bytes());
            out.extend_from_slice(&stat.max.to_le_bytes());
            let mut flags = 0_u8;
            if stat.has_value {
                flags |= 1;
            }
            if stat.has_null {
                flags |= 2;
            }
            out.push(flags);
        }
    }
    out
}

/// 编码 `ttl_map`(每块 `min(expires_at)`;无 TTL 行记 `i64::MAX` = +∞)。
///
/// 紧接 zone map 数据之后存放(计入 `zmap_len`)。块内 `min > now` 是"整块记录
/// 均未过期"的充分条件(无 TTL 行视为 +∞),查询期据此免逐行 TTL 判定。
pub(crate) fn encode_ttl_map(min_expires: &[i64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(min_expires.len() * 8);
    for value in min_expires {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// 解码 `ttl_map`(须给定与编码一致的字段字典与块数)。
///
/// # Errors
/// zone map 区结构不符、尾部长度不等于 `block_count × 8` 时返回
/// [`MnemeError::Corrupted`]。
pub(crate) fn decode_ttl_map(
    bytes: &[u8],
    fields: &[FieldDef],
    block_count: usize,
) -> Result<Vec<i64>> {
    let mut cursor = Cursor::new(bytes, "msec zmap");
    let field_count = cursor.u32()? as usize;
    let expected_fields = fields
        .iter()
        .filter(|field| field.kind != FieldKind::Str)
        .count();
    if field_count != expected_fields {
        return Err(corrupted("zmap: 字段数与 field_dict 不符"));
    }
    let stored_blocks = cursor.u64()? as usize;
    if stored_blocks != block_count {
        return Err(corrupted("zmap: 块数与段行数不符"));
    }
    // 跳过 zone map 数据区(field_id + 每块 17 B)。
    for _ in 0..field_count {
        let _ = cursor.u32()?;
        for _ in 0..block_count {
            let _ = cursor.take(17)?;
        }
    }
    let tail = cursor.take(cursor.remaining())?;
    if tail.len() != block_count * 8 {
        return Err(corrupted("zmap: ttl_map 尾部长度不符"));
    }
    Ok(tail
        .chunks_exact(8)
        .map(|chunk| i64::from_le_bytes(chunk.try_into().unwrap_or([0; 8])))
        .collect())
}

/// 校验 zone map 区结构:字段数/块数与字段字典及段行数一致,统计布局完整。
pub(crate) fn validate_zmap(bytes: &[u8], fields: &[FieldDef], block_count: usize) -> Result<()> {
    let expected_fields = fields
        .iter()
        .filter(|field| field.kind != FieldKind::Str)
        .count();
    let mut cursor = Cursor::new(bytes, "msec zmap");
    let field_count = cursor.u32()? as usize;
    if field_count != expected_fields {
        return Err(corrupted("zmap: 字段数与 field_dict 不符"));
    }
    let stored_blocks = cursor.u64()? as usize;
    if stored_blocks != block_count {
        return Err(corrupted("zmap: 块数与段行数不符"));
    }
    for _ in 0..field_count {
        let field_id = cursor.u32()? as usize;
        if field_id >= fields.len() || fields[field_id].kind == FieldKind::Str {
            return Err(corrupted("zmap: 字段编号非法"));
        }
        for _ in 0..block_count {
            let min = f64::from_le_bytes(
                cursor
                    .take(8)?
                    .try_into()
                    .map_err(|_| corrupted("zmap: 区间字节不足"))?,
            );
            let max = f64::from_le_bytes(
                cursor
                    .take(8)?
                    .try_into()
                    .map_err(|_| corrupted("zmap: 区间字节不足"))?,
            );
            let flags = cursor.u8()?;
            if flags & !0b11 != 0 {
                return Err(corrupted("zmap: 统计标志位非法"));
            }
            // ±∞ 是合法的"区间未知"编码(超 `f64` 精确范围的值放弃剪枝);
            // NaN 与 min > max 仍然拒绝。
            if min.is_nan() || max.is_nan() || min > max {
                return Err(corrupted("zmap: 区间非法"));
            }
        }
    }
    let tail = cursor.remaining();
    // 尾部必须恰为 `block_count × 8` 字节的 min(expires_at)。
    if tail != block_count * 8 {
        return Err(corrupted("zmap: ttl_map 尾部长度不符"));
    }
    Ok(())
}
