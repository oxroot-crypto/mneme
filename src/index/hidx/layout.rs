use crate::core::error::Result;
use crate::persist::{Cursor, crc32};

use super::encode::{HEADER_LEN, NODE_TABLE_ENTRY};
use super::header::{HidxHeader, corrupt};

/// 最高层级硬上限(防篡改导致巨量分配)。
const MAX_LEVEL: u8 = 64;

/// 校验长度与负载 CRC,返回 `(数据区起点, 邻接区起点, 负载终点)`。
pub(super) fn validate_layout(bytes: &[u8], header: &HidxHeader) -> Result<(usize, usize, usize)> {
    let expected_node_table = header
        .count
        .checked_mul(NODE_TABLE_ENTRY)
        .ok_or_else(|| corrupt("node_table_len 溢出"))?;
    if header.node_table_len != expected_node_table {
        return Err(corrupt("node_table_len 与 count 不符"));
    }
    let body_start = HEADER_LEN as usize;
    let adj_start = body_start
        .checked_add(header.node_table_len)
        .ok_or_else(|| corrupt("长度溢出"))?;
    let payload_end = adj_start
        .checked_add(header.adj_len)
        .ok_or_else(|| corrupt("长度溢出"))?;
    if payload_end
        .checked_add(4)
        .is_none_or(|end| end != bytes.len())
    {
        return Err(corrupt("文件总长与头部不符"));
    }
    let stored_payload_crc = u32::from_le_bytes([
        bytes[payload_end],
        bytes[payload_end + 1],
        bytes[payload_end + 2],
        bytes[payload_end + 3],
    ]);
    if crc32(&bytes[body_start..payload_end]) != stored_payload_crc {
        return Err(corrupt("payload_crc32 不符"));
    }
    Ok((body_start, adj_start, payload_end))
}

/// 解码节点表,返回 `(levels, offsets)`。
pub(super) fn read_node_table(node_table: &[u8], count: usize) -> Result<(Vec<u8>, Vec<usize>)> {
    let mut levels = Vec::with_capacity(count);
    let mut offsets = Vec::with_capacity(count);
    for node in 0..count {
        let base = node * NODE_TABLE_ENTRY;
        let level = node_table[base];
        if level > MAX_LEVEL {
            return Err(corrupt("层级越界"));
        }
        let offset = u32::from_le_bytes([
            node_table[base + 1],
            node_table[base + 2],
            node_table[base + 3],
            node_table[base + 4],
        ]) as usize;
        levels.push(level);
        offsets.push(offset);
    }
    Ok((levels, offsets))
}

/// 校验入口点与层级一致性。
///
/// 自产图的不变量:入口节点即全图最高层节点(`link_node` 仅在更高层出现时替换入口),
/// 载入路径必须同口径强制(FC-INDEX-INV-007);层级不等一律拒绝。
pub(super) fn validate_entry(
    levels: &[u8],
    count: usize,
    entry_slot: u32,
    entry_level: u8,
) -> Result<()> {
    let max_level = levels.iter().copied().max().unwrap_or(0);
    if entry_level != max_level {
        return Err(corrupt("入口层级不等于全图最高层"));
    }
    if count > 0 {
        if entry_slot as usize >= count {
            return Err(corrupt("入口槽位越界"));
        }
        if levels[entry_slot as usize] != entry_level {
            return Err(corrupt("入口层级与节点层级不一致"));
        }
    }
    Ok(())
}

/// `Cursor` 当前位置(邻接区相对偏移)= 总长 - 剩余。
pub(super) fn cursor_position(cursor: &Cursor<'_>, total: usize) -> usize {
    total - cursor.remaining()
}
