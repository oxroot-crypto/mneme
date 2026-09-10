//! `hidx`(HNSW 图段)字节布局(设计 05 §10)。
//!
//! ```text
//! 0   magic "HID1" | 4 u16 ver | 6 u16 header_len | 8 u16 m | 10 u16 m0
//! 12  u16 ef_construction | 14 u8 entry_level | 15 u8 reserved | 16 f32 ml
//! 20  u32 count | 24 u32 entry_slot | 28 u32 node_table_len | 32 u32 adj_len
//! 36  u32 header_crc32(覆盖 [0,36)) | 40..64 padding
//! node_table(64 起,count × 5 B): (u8 level, u32 adj_off)   # adj_off 相对 adj_blob 起点
//! adj_blob: 逐节点逐层 [u16 degree][degree × u32 邻居节点 id]
//! 尾部:     u32 payload_crc32(覆盖 node_table || adj_blob)
//! ```
//!
//! 整数小端。节点 id 为段内槽位下标;载入后按恢复重排映射到全局槽位。

use crate::core::error::{MnemeError, Result};
use crate::persist::{Cursor, FORMAT_VERSION, check_version, crc32, put_u16, put_u32};

use super::graph::Graph;

/// hidx 魔数。
pub(crate) const MAGIC: [u8; 4] = *b"HID1";
/// 定长头部长度(字节)。
pub(crate) const HEADER_LEN: u16 = 64;
/// 单节点表条目字节数(`u8 level` + `u32 adj_off`)。
pub(crate) const NODE_TABLE_ENTRY: usize = 5;
/// 单节点单层邻接表硬上限(防篡改导致巨量分配);与配置校验共用。
const MAX_DEGREE: usize = crate::memory::index::MAX_INDEX_DEGREE as usize;
/// 最高层级硬上限(防篡改导致巨量分配)。
const MAX_LEVEL: u8 = 64;

/// 一个已解码的 hidx 图与其构建参数。
pub(crate) struct Decoded {
    /// 邻接图(节点 id = 段内槽位)。
    pub(crate) graph: Graph,
    /// 上层度数上限。
    pub(crate) m: u16,
    /// 第 0 层度数上限。
    pub(crate) m0: u16,
    /// 构建期探查宽度。
    pub(crate) ef_construction: u16,
    /// 层级骰子系数。
    pub(crate) ml: f32,
}

/// 编码邻接图为 hidx 字节。
pub(crate) fn encode(graph: &Graph, m: u16, m0: u16, ef_construction: u16, ml: f32) -> Vec<u8> {
    let count = graph.node_count();
    let mut node_table = Vec::with_capacity(count * NODE_TABLE_ENTRY);
    let mut adj_blob: Vec<u8> = Vec::new();
    for node in 0..count {
        let level = graph.levels[node];
        node_table.push(level);
        put_u32(&mut node_table, adj_blob.len() as u32);
        for layer in 0..=level as usize {
            let neighbors = graph.neighbors(node as u32, layer);
            put_u16(&mut adj_blob, neighbors.len() as u16);
            for &neighbor in neighbors {
                put_u32(&mut adj_blob, neighbor);
            }
        }
    }

    let mut header = [0_u8; HEADER_LEN as usize];
    header[0..4].copy_from_slice(&MAGIC);
    header[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    header[6..8].copy_from_slice(&HEADER_LEN.to_le_bytes());
    header[8..10].copy_from_slice(&m.to_le_bytes());
    header[10..12].copy_from_slice(&m0.to_le_bytes());
    header[12..14].copy_from_slice(&ef_construction.to_le_bytes());
    header[14] = graph.entry_level;
    header[16..20].copy_from_slice(&ml.to_le_bytes());
    header[20..24].copy_from_slice(&(count as u32).to_le_bytes());
    header[24..28].copy_from_slice(&graph.entry.to_le_bytes());
    header[28..32].copy_from_slice(&(node_table.len() as u32).to_le_bytes());
    header[32..36].copy_from_slice(&(adj_blob.len() as u32).to_le_bytes());
    let crc = crc32(&header[0..36]);
    header[36..40].copy_from_slice(&crc.to_le_bytes());

    let mut out = Vec::with_capacity(HEADER_LEN as usize + node_table.len() + adj_blob.len() + 4);
    out.extend_from_slice(&header);
    out.extend_from_slice(&node_table);
    out.extend_from_slice(&adj_blob);
    let mut payload = Vec::with_capacity(node_table.len() + adj_blob.len());
    payload.extend_from_slice(&node_table);
    payload.extend_from_slice(&adj_blob);
    out.extend_from_slice(&crc32(&payload).to_le_bytes());
    out
}

/// hidx 定长头部字段(解码中间态)。
struct HidxHeader {
    m: u16,
    m0: u16,
    ef_construction: u16,
    entry_level: u8,
    entry_slot: u32,
    count: usize,
    ml: f32,
    node_table_len: usize,
    adj_len: usize,
}

/// 解码并校验 hidx 字节。
///
/// # Errors
/// 魔数/版本/`header_len`/头部 CRC/长度/负载 CRC 不符,或度数/邻居 id 越界时
/// 返回 [`MnemeError::Corrupted`]/[`MnemeError::UnsupportedVersion`],绝不 panic。
pub(crate) fn decode(bytes: &[u8]) -> Result<Decoded> {
    let header = parse_header(bytes)?;
    let (body_start, adj_start, payload_end) = validate_layout(bytes, &header)?;
    let node_table = &bytes[body_start..adj_start];
    let adj_blob = &bytes[adj_start..payload_end];
    let (levels, offsets) = read_node_table(node_table, header.count)?;
    let graph = build_graph(
        adj_blob,
        &levels,
        &offsets,
        header.count,
        header.entry_slot,
        header.entry_level,
    )?;
    Ok(Decoded {
        graph,
        m: header.m,
        m0: header.m0,
        ef_construction: header.ef_construction,
        ml: header.ml,
    })
}

/// 校验魔数/版本/头部 CRC 并读取定长头部。
fn parse_header(bytes: &[u8]) -> Result<HidxHeader> {
    if bytes.len() < HEADER_LEN as usize {
        return Err(corrupt("文件短于头部"));
    }
    if bytes[0..4] != MAGIC {
        return Err(corrupt("魔数不符"));
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    check_version("hidx", version)?;
    let header_len = u16::from_le_bytes([bytes[6], bytes[7]]);
    if header_len != HEADER_LEN {
        return Err(corrupt("header_len 不符"));
    }
    let stored_crc = u32::from_le_bytes([bytes[36], bytes[37], bytes[38], bytes[39]]);
    if crc32(&bytes[0..36]) != stored_crc {
        return Err(corrupt("header_crc32 不符"));
    }

    let mut cursor = Cursor::new(bytes, "hidx 头部");
    let _magic = cursor.take(4)?;
    let _version = cursor.u16()?;
    let _header_len = cursor.u16()?;
    let m = cursor.u16()?;
    let m0 = cursor.u16()?;
    let ef_construction = cursor.u16()?;
    let entry_level = cursor.u8()?;
    let _reserved = cursor.u8()?;
    let ml = f32::from_le_bytes([cursor.u8()?, cursor.u8()?, cursor.u8()?, cursor.u8()?]);
    let count = cursor.u32()? as usize;
    let entry_slot = cursor.u32()?;
    let node_table_len = cursor.u32()? as usize;
    let adj_len = cursor.u32()? as usize;
    // 头部图参数须落在构建期允许域内(与 `Builder::validate` 同口径),
    // 否则自产文件与手改文件口径不一致。
    if m < 2 || m0 < m || m as usize > MAX_DEGREE || m0 as usize > MAX_DEGREE {
        return Err(corrupt("头部度数参数越界"));
    }
    if !ml.is_finite() || ml <= 0.0 {
        return Err(corrupt("ml 非正有限值"));
    }
    Ok(HidxHeader {
        m,
        m0,
        ef_construction,
        entry_level,
        entry_slot,
        count,
        ml,
        node_table_len,
        adj_len,
    })
}

/// 校验长度与负载 CRC,返回 `(数据区起点, 邻接区起点, 负载终点)`。
fn validate_layout(bytes: &[u8], header: &HidxHeader) -> Result<(usize, usize, usize)> {
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
fn read_node_table(node_table: &[u8], count: usize) -> Result<(Vec<u8>, Vec<usize>)> {
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
fn validate_entry(levels: &[u8], count: usize, entry_slot: u32, entry_level: u8) -> Result<()> {
    let max_level = levels.iter().copied().max().unwrap_or(0);
    if entry_level > max_level {
        return Err(corrupt("入口层级超过最高层"));
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

/// 校验入口并解码邻接区为图。
fn build_graph(
    adj_blob: &[u8],
    levels: &[u8],
    offsets: &[usize],
    count: usize,
    entry_slot: u32,
    entry_level: u8,
) -> Result<Graph> {
    validate_entry(levels, count, entry_slot, entry_level)?;
    let mut graph = Graph::new();
    for &level in levels {
        graph.push_node(level);
    }
    graph.entry = entry_slot;
    graph.entry_level = entry_level;

    let mut cursor = Cursor::new(adj_blob, "hidx 邻接区");
    for node in 0..count {
        if offsets[node] != cursor_position(&cursor, adj_blob.len()) {
            return Err(corrupt("adj_off 与逐节点布局不符"));
        }
        let level = levels[node];
        for layer in 0..=level as usize {
            let degree = cursor.u16()? as usize;
            if degree > MAX_DEGREE {
                return Err(corrupt("度数越界"));
            }
            let mut neighbors = Vec::with_capacity(degree.min(NEIGHBOR_PREALLOC));
            for _ in 0..degree {
                let neighbor = cursor.u32()?;
                if count > 0 && neighbor as usize >= count {
                    return Err(corrupt("邻居 id 越界"));
                }
                if neighbor as usize == node {
                    return Err(corrupt("存在自环"));
                }
                neighbors.push(neighbor);
            }
            graph.set_neighbors(node as u32, layer, neighbors);
        }
    }
    if !cursor.is_empty() {
        return Err(corrupt("邻接区存在多余字节"));
    }
    Ok(graph)
}

/// 邻接区每层预分配的保守上界(仅用于 `Vec::with_capacity`)。
const NEIGHBOR_PREALLOC: usize = 8;

/// `Cursor` 当前位置(邻接区相对偏移)= 总长 - 剩余。
fn cursor_position(cursor: &Cursor<'_>, total: usize) -> usize {
    total - cursor.remaining()
}

/// 构造 hidx 文件级损坏错误。
fn corrupt(reason: &str) -> MnemeError {
    MnemeError::Corrupted {
        segment: None,
        reason: format!("hidx: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::graph::Graph;

    fn sample_graph() -> Graph {
        let mut graph = Graph::new();
        graph.push_node(1);
        graph.push_node(0);
        graph.push_node(1);
        graph.add_neighbor(0, 0, 1);
        graph.add_neighbor(1, 0, 0);
        graph.add_neighbor(0, 1, 2);
        graph.add_neighbor(2, 1, 0);
        graph.add_neighbor(1, 1, 2);
        graph.add_neighbor(2, 1, 1);
        graph.entry = 0;
        graph.entry_level = 1;
        graph
    }

    /// FC-INDEX-POST-007:hidx 编解码往返恢复同一图(层级/邻接/入口/参数)。
    #[test]
    fn hidx_roundtrip_restores_graph() {
        let graph = sample_graph();
        let bytes = encode(&graph, 16, 32, 200, 0.5);
        let decoded = decode(&bytes).expect("decode");
        assert_eq!(decoded.graph.node_count(), 3);
        assert_eq!(decoded.graph.entry, 0);
        assert_eq!(decoded.graph.entry_level, 1);
        assert_eq!(decoded.m, 16);
        assert_eq!(decoded.m0, 32);
        assert_eq!(decoded.ef_construction, 200);
        assert!((decoded.ml - 0.5).abs() < 1e-6);
        for node in 0..3u32 {
            for layer in 0..=graph.levels[node as usize] as usize {
                assert_eq!(
                    decoded.graph.neighbors(node, layer),
                    graph.neighbors(node, layer)
                );
            }
        }
    }

    /// FC-INDEX-ERR-001:魔数不符 → `Corrupted`。
    #[test]
    fn hidx_rejects_bad_magic() {
        let mut bytes = encode(&sample_graph(), 16, 32, 200, 0.5);
        bytes[0] = b'X';
        assert!(matches!(decode(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// FC-INDEX-ERR-001:负载 CRC 翻转 → `Corrupted`。
    #[test]
    fn hidx_detects_payload_corruption() {
        let mut bytes = encode(&sample_graph(), 16, 32, 200, 0.5);
        let last = bytes.len() - 5;
        bytes[last] ^= 0x01;
        assert!(matches!(decode(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// FC-INDEX-ERR-001:更高主版本 → `UnsupportedVersion`(I18)。
    #[test]
    fn hidx_rejects_higher_major() {
        let mut bytes = encode(&sample_graph(), 16, 32, 200, 0.5);
        bytes[4..6].copy_from_slice(&0x0100_u16.to_le_bytes());
        let crc = crc32(&bytes[0..36]);
        bytes[36..40].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            decode(&bytes),
            Err(MnemeError::UnsupportedVersion { .. })
        ));
    }

    /// FC-INDEX-CPLX-004:带边图的编解码规模随节点数近似线性。
    #[test]
    fn hidx_encode_decode_scale_linearly() {
        fn chain(count: usize) -> Graph {
            let mut graph = Graph::new();
            for _ in 0..count {
                graph.push_node(0);
            }
            for node in 1..count as u32 {
                graph.add_neighbor(node, 0, node - 1);
                graph.add_neighbor(node - 1, 0, node);
            }
            graph
        }
        let small_len = encode(&chain(50), 16, 32, 200, 0.5).len();
        let large_len = encode(&chain(200), 16, 32, 200, 0.5).len();
        let decoded = decode(&encode(&chain(200), 16, 32, 200, 0.5)).expect("decode");
        assert_eq!(decoded.graph.node_count(), 200);
        // 邻接也被完整恢复(链中间的节点有左右两条边)。
        assert_eq!(decoded.graph.neighbors(100, 0).len(), 2);
        let ratio = large_len as f64 / small_len as f64;
        assert!(ratio < 6.0, "hidx 长度增长过快(疑似超线性):{ratio}");
    }
}
