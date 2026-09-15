use crate::core::error::Result;
use crate::index::graph::MappedGraph;
use crate::memory::lazy::ByteSpan;
use crate::persist::Cursor;

// `Graph` 仅被测试路径(`Decoded`/`build_graph`)使用。
#[cfg(test)]
use crate::index::graph::Graph;
// `MnemeError` 仅被 `decode` 的 rustdoc 链接引用,导入以保链接可解析。
#[cfg(test)]
#[allow(unused_imports)]
use crate::core::error::MnemeError;

use super::header::{HidxHeader, corrupt, parse_header};
use super::layout::{cursor_position, read_node_table, validate_entry, validate_layout};

/// 一个已解码的 hidx 图与其构建参数(测试与整体往返用)。
#[cfg(test)]
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

/// 校验 hidx 字节(不构建图,供 `IndexFactory::verify`/`check()` 使用)。
///
/// 校验强度与 [`decode`] 相同:头部/版本/布局/入口/邻接逐节点结构与内容。
///
/// # Errors
/// 同 [`decode`]。
pub(crate) fn verify(bytes: &[u8]) -> Result<()> {
    let header = parse_header(bytes)?;
    let (body_start, adj_start, payload_end) = validate_layout(bytes, &header)?;
    let (levels, offsets) = read_node_table(&bytes[body_start..adj_start], header.count)?;
    validate_entry(&levels, header.count, header.entry_slot, header.entry_level)?;
    validate_adjacency(&header, &bytes[adj_start..payload_end], &levels, &offsets)
}

/// 解码并校验 hidx 字节为内存图(测试与整体往返用;生产载入走 [`open`])。
///
/// # Errors
/// 魔数/版本/`header_len`/头部 CRC/长度/负载 CRC 不符,或度数/邻居 id 越界时
/// 返回 [`MnemeError::Corrupted`]/[`MnemeError::UnsupportedVersion`],绝不 panic。
#[cfg(test)]
pub(crate) fn decode(bytes: &[u8]) -> Result<Decoded> {
    let header = parse_header(bytes)?;
    let (body_start, adj_start, payload_end) = validate_layout(bytes, &header)?;
    let node_table = &bytes[body_start..adj_start];
    let adj_blob = &bytes[adj_start..payload_end];
    let (levels, offsets) = read_node_table(node_table, header.count)?;
    let graph = build_graph(&header, adj_blob, &levels, &offsets)?;
    Ok(Decoded {
        graph,
        m: header.m,
        m0: header.m0,
        ef_construction: header.ef_construction,
        ml: header.ml,
    })
}

/// 打开 hidx 为**惰性图**:只解析头部与 `node_table` 并做一次无分配的
/// 邻接区结构校验,邻接字节留给 [`MappedGraph`] 按需解码。
///
/// 与 [`decode`] 同校验强度(度数上界 / 邻居越界 / 自环 / 布局逐节点吻合),
/// 但不在打开期分配逐节点邻居表(FC-PERSIST-INV-021);整文件 CRC 由段句柄
/// 打开期校验(`SegmentHandle::open`)。
///
/// # Errors
/// 同 [`decode`]。
pub(crate) fn open(span: &ByteSpan) -> Result<MappedGraph> {
    let bytes = span
        .slice_all()
        .ok_or_else(|| corrupt("hidx 句柄切片失败"))?;
    let header = parse_header(bytes)?;
    let (body_start, adj_start, payload_end) = validate_layout(bytes, &header)?;
    let node_table = &bytes[body_start..adj_start];
    let (levels, offsets) = read_node_table(node_table, header.count)?;
    validate_entry(&levels, header.count, header.entry_slot, header.entry_level)?;
    validate_adjacency(&header, &bytes[adj_start..payload_end], &levels, &offsets)?;
    Ok(MappedGraph::new(crate::index::graph::MappedGraphParts {
        span: span.clone(),
        count: header.count,
        levels,
        offsets,
        adj_start,
        params: crate::index::graph::GraphParams {
            m: header.m,
            m0: header.m0,
            ef_construction: header.ef_construction,
            ml: header.ml,
        },
        entry: header.entry_slot,
        entry_level: header.entry_level,
    }))
}

/// 无分配地校验邻接区布局与内容(与 [`build_graph`] 同口径)。
fn validate_adjacency(
    header: &HidxHeader,
    adj_blob: &[u8],
    levels: &[u8],
    offsets: &[usize],
) -> Result<()> {
    let mut cursor = Cursor::new(adj_blob, "hidx 邻接区");
    for node in 0..header.count {
        if offsets[node] != cursor_position(&cursor, adj_blob.len()) {
            return Err(corrupt("adj_off 与逐节点布局不符"));
        }
        let level = levels[node];
        for layer in 0..=level as usize {
            let bound = if layer == 0 {
                header.m0 as usize
            } else {
                header.m as usize
            };
            let degree = cursor.u16()? as usize;
            if degree > bound {
                return Err(corrupt("该层度数超过 M0/M 上界"));
            }
            for _ in 0..degree {
                let neighbor = cursor.u32()?;
                if neighbor as usize >= header.count {
                    return Err(corrupt("邻居 id 越界"));
                }
                if neighbor as usize == node {
                    return Err(corrupt("存在自环"));
                }
            }
        }
    }
    if !cursor.is_empty() {
        return Err(corrupt("邻接区存在多余字节"));
    }
    Ok(())
}

/// 校验入口并解码邻接区为图(测试路径;生产载入走 [`open`])。
#[cfg(test)]
fn build_graph(
    header: &HidxHeader,
    adj_blob: &[u8],
    levels: &[u8],
    offsets: &[usize],
) -> Result<Graph> {
    validate_entry(levels, header.count, header.entry_slot, header.entry_level)?;
    let mut graph = Graph::new();
    for &level in levels {
        graph.push_node(level);
    }
    graph.entry = header.entry_slot;
    graph.entry_level = header.entry_level;

    let mut cursor = Cursor::new(adj_blob, "hidx 邻接区");
    for node in 0..header.count {
        if offsets[node] != cursor_position(&cursor, adj_blob.len()) {
            return Err(corrupt("adj_off 与逐节点布局不符"));
        }
        let level = levels[node];
        for layer in 0..=level as usize {
            let bound = if layer == 0 {
                header.m0 as usize
            } else {
                header.m as usize
            };
            let neighbors = read_neighbors(&mut cursor, node, header.count, bound)?;
            graph.set_neighbors(node as u32, layer, neighbors);
        }
    }
    if !cursor.is_empty() {
        return Err(corrupt("邻接区存在多余字节"));
    }
    Ok(graph)
}

/// 读取一个节点在一层的邻接表:度数 ≤ 该层上界(`M0`/`M`)、邻居 id 合法且无自环。
#[cfg(test)]
fn read_neighbors(
    cursor: &mut Cursor<'_>,
    node: usize,
    count: usize,
    bound: usize,
) -> Result<Vec<u32>> {
    let degree = cursor.u16()? as usize;
    if degree > bound {
        return Err(corrupt("该层度数超过 M0/M 上界"));
    }
    let mut neighbors = Vec::with_capacity(degree.min(NEIGHBOR_PREALLOC));
    for _ in 0..degree {
        let neighbor = cursor.u32()?;
        if neighbor as usize >= count {
            return Err(corrupt("邻居 id 越界"));
        }
        if neighbor as usize == node {
            return Err(corrupt("存在自环"));
        }
        neighbors.push(neighbor);
    }
    Ok(neighbors)
}

/// 邻接区每层预分配的保守上界(仅用于测试路径的 `Vec::with_capacity`)。
#[cfg(test)]
const NEIGHBOR_PREALLOC: usize = 8;
