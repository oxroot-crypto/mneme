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

/// hidx 头部携带的图构建参数(`ef_search` 不入文件,见设计 05 §10)。
#[derive(Debug, Clone, Copy)]
pub(crate) struct GraphParams {
    /// 上层度数上限。
    pub(crate) m: u16,
    /// 第 0 层度数上限。
    pub(crate) m0: u16,
    /// 构建期探查宽度。
    pub(crate) ef_construction: u16,
    /// 层级骰子系数。
    pub(crate) ml: f32,
}

/// 编码时已确定的长度字段(节点数 / 节点表字节数 / 邻接区字节数,均 `u32`)。
#[derive(Clone, Copy)]
struct EncodedLengths {
    nodes: u32,
    node_table: u32,
    adj: u32,
}

/// 编码邻接图为 hidx 字节。
///
/// # Errors
/// 节点数/邻接区超过格式的 `u32` 长度上限,或单层度数超过 `u16` 时返回结构化错误
/// (绝不静默截断;`Builder` 校验下正常构建不可达)。
pub(crate) fn encode(graph: &Graph, params: GraphParams) -> Result<Vec<u8>> {
    let count = graph.node_count();
    let mut node_table = Vec::with_capacity(count * NODE_TABLE_ENTRY);
    let mut adj_blob: Vec<u8> = Vec::new();
    for node in 0..count {
        let level = graph.levels[node];
        node_table.push(level);
        put_u32(
            &mut node_table,
            u32::try_from(adj_blob.len())
                .map_err(|_| encode_too_large("hidx 邻接区字节数", adj_blob.len()))?,
        );
        for layer in 0..=level as usize {
            let neighbors = graph.neighbors(node as u32, layer);
            let degree = u16::try_from(neighbors.len()).map_err(|_| MnemeError::Inconsistent {
                reason: "hidx: 单层度数超过 u16(违反度数上界不变量)",
            })?;
            put_u16(&mut adj_blob, degree);
            for &neighbor in neighbors {
                put_u32(&mut adj_blob, neighbor);
            }
        }
    }

    let lengths = EncodedLengths {
        nodes: u32::try_from(count).map_err(|_| encode_too_large("hidx 节点数", count))?,
        node_table: u32::try_from(node_table.len())
            .map_err(|_| encode_too_large("hidx 节点表字节数", node_table.len()))?,
        adj: u32::try_from(adj_blob.len())
            .map_err(|_| encode_too_large("hidx 邻接区字节数", adj_blob.len()))?,
    };
    let header = encode_header(graph, params, lengths);

    let mut out = Vec::with_capacity(HEADER_LEN as usize + node_table.len() + adj_blob.len() + 4);
    out.extend_from_slice(&header);
    out.extend_from_slice(&node_table);
    out.extend_from_slice(&adj_blob);
    out.extend_from_slice(&crc32(&out[HEADER_LEN as usize..]).to_le_bytes());
    Ok(out)
}

/// 组装 hidx 定长头部(含头部 CRC)。
fn encode_header(
    graph: &Graph,
    params: GraphParams,
    lengths: EncodedLengths,
) -> [u8; HEADER_LEN as usize] {
    let mut header = [0_u8; HEADER_LEN as usize];
    header[0..4].copy_from_slice(&MAGIC);
    header[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    header[6..8].copy_from_slice(&HEADER_LEN.to_le_bytes());
    header[8..10].copy_from_slice(&params.m.to_le_bytes());
    header[10..12].copy_from_slice(&params.m0.to_le_bytes());
    header[12..14].copy_from_slice(&params.ef_construction.to_le_bytes());
    header[14] = graph.entry_level;
    header[16..20].copy_from_slice(&params.ml.to_le_bytes());
    header[20..24].copy_from_slice(&lengths.nodes.to_le_bytes());
    header[24..28].copy_from_slice(&graph.entry.to_le_bytes());
    header[28..32].copy_from_slice(&lengths.node_table.to_le_bytes());
    header[32..36].copy_from_slice(&lengths.adj.to_le_bytes());
    let crc = crc32(&header[0..36]);
    header[36..40].copy_from_slice(&crc.to_le_bytes());
    header
}

/// 构造"长度字段超出格式上限"的结构化错误(拒绝静默截断)。
fn encode_too_large(what: &'static str, got: usize) -> MnemeError {
    MnemeError::LimitExceeded {
        field: what,
        limit: u32::MAX as usize,
        got,
    }
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
    let graph = build_graph(&header, adj_blob, &levels, &offsets)?;
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
    validate_header_params(m, m0, ef_construction, ml)?;
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

/// 校验头部图参数落在构建期允许域内(与 `Builder::validate` 同口径);
/// 否则自产文件与手改文件口径不一致。
fn validate_header_params(m: u16, m0: u16, ef_construction: u16, ml: f32) -> Result<()> {
    if m < 2 || m0 < m || m as usize > MAX_DEGREE || m0 as usize > MAX_DEGREE {
        return Err(corrupt("头部度数参数越界"));
    }
    if ef_construction < 1 {
        return Err(corrupt("ef_construction 必须 ≥ 1"));
    }
    if !ml.is_finite() || ml <= 0.0 {
        return Err(corrupt("ml 非正有限值"));
    }
    Ok(())
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
///
/// 自产图的不变量:入口节点即全图最高层节点(`link_node` 仅在更高层出现时替换入口),
/// 载入路径必须同口径强制(FC-INDEX-INV-007);层级不等一律拒绝。
fn validate_entry(levels: &[u8], count: usize, entry_slot: u32, entry_level: u8) -> Result<()> {
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

/// 校验入口并解码邻接区为图。
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
    use proptest::prelude::*;

    /// 常规图参数(m=16/m0=32/efc=200/ml=0.5),供往返与损坏测试复用。
    const GRAPH_PARAMS: GraphParams = GraphParams {
        m: 16,
        m0: 32,
        ef_construction: 200,
        ml: 0.5,
    };

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
        graph.entry = 2;
        graph.entry_level = 1;
        graph
    }

    /// 节点 0 在第 0 层连 3 个邻居的"超上界"图(m0=2 时违反度数上界)。
    fn hub_graph(layer: usize) -> Graph {
        let mut graph = Graph::new();
        for _ in 0..4 {
            graph.push_node(layer as u8);
        }
        for neighbor in 1..4u32 {
            graph.add_neighbor(0, layer, neighbor);
            graph.add_neighbor(neighbor, layer, 0);
        }
        graph.entry = 0;
        graph.entry_level = layer as u8;
        graph
    }

    /// FC-INDEX-POST-007:hidx 编解码往返恢复同一图(层级/邻接/入口/参数)。
    #[test]
    fn hidx_roundtrip_restores_graph() {
        let graph = sample_graph();
        let bytes = encode(&graph, GRAPH_PARAMS).expect("encode");
        let decoded = decode(&bytes).expect("decode");
        // 黄金头部:逐字段钉死字节布局(设计 05 §10),防止 encode/decode 同错同过。
        assert_eq!(&bytes[0..4], b"HID1");
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), FORMAT_VERSION);
        assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), HEADER_LEN);
        assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), GRAPH_PARAMS.m);
        assert_eq!(u16::from_le_bytes([bytes[10], bytes[11]]), GRAPH_PARAMS.m0);
        assert_eq!(bytes[14], graph.entry_level);
        assert_eq!(
            u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
            3
        );
        assert_eq!(
            u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
            graph.entry
        );
        assert_eq!(
            u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]),
            (3 * NODE_TABLE_ENTRY) as u32
        );
        assert_eq!(
            u32::from_le_bytes([bytes[36], bytes[37], bytes[38], bytes[39]]),
            crc32(&bytes[0..36])
        );
        assert_eq!(decoded.graph.node_count(), 3);
        assert_eq!(decoded.graph.entry, 2);
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
        let mut bytes = encode(&sample_graph(), GRAPH_PARAMS).expect("encode");
        bytes[0] = b'X';
        assert!(matches!(decode(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// FC-INDEX-ERR-001:负载 CRC 翻转 → `Corrupted`。
    #[test]
    fn hidx_detects_payload_corruption() {
        let mut bytes = encode(&sample_graph(), GRAPH_PARAMS).expect("encode");
        let last = bytes.len() - 5;
        bytes[last] ^= 0x01;
        assert!(matches!(decode(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// FC-INDEX-ERR-001:更高主版本 → `UnsupportedVersion`(I18)。
    #[test]
    fn hidx_rejects_higher_major() {
        let mut bytes = encode(&sample_graph(), GRAPH_PARAMS).expect("encode");
        bytes[4..6].copy_from_slice(&0x0200_u16.to_le_bytes());
        let crc = crc32(&bytes[0..36]);
        bytes[36..40].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            decode(&bytes),
            Err(MnemeError::UnsupportedVersion { .. })
        ));
    }

    /// FC-INDEX-ERR-001:头部 `ef_construction = 0` → `Corrupted`(与建库校验同口径)。
    #[test]
    fn hidx_rejects_zero_ef_construction() {
        let bytes = encode(
            &sample_graph(),
            GraphParams {
                ef_construction: 0,
                ..GRAPH_PARAMS
            },
        )
        .expect("encode");
        assert!(matches!(decode(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// FC-INDEX-INV-007:第 0 层度数超过 `M0` 或上层超过 `M` → `Corrupted`。
    #[test]
    fn hidx_rejects_degree_above_layer_bound() {
        // 第 0 层度 3 > M0 = 2。
        let layer0 = encode(
            &hub_graph(0),
            GraphParams {
                m: 2,
                m0: 2,
                ef_construction: 1,
                ..GRAPH_PARAMS
            },
        )
        .expect("encode");
        assert!(matches!(decode(&layer0), Err(MnemeError::Corrupted { .. })));
        // 第 1 层度 3 > M = 2。
        let layer1 = encode(
            &hub_graph(1),
            GraphParams {
                m: 2,
                m0: 2,
                ef_construction: 1,
                ..GRAPH_PARAMS
            },
        )
        .expect("encode");
        assert!(matches!(decode(&layer1), Err(MnemeError::Corrupted { .. })));
    }

    /// FC-INDEX-INV-007:入口层级低于全图最高层 → `Corrupted`(入口必须是最高活层节点)。
    #[test]
    fn hidx_rejects_entry_level_below_max() {
        let mut graph = Graph::new();
        graph.push_node(1);
        graph.push_node(0);
        graph.entry = 1;
        graph.entry_level = 0;
        let bytes = encode(&graph, GRAPH_PARAMS).expect("encode");
        assert!(matches!(decode(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// FC-INDEX-ERR-003:单层度数超过 `u16` 表示范围 → `Inconsistent`(编码拒绝
    /// 静默截断;`Builder` 校验下正常构建不可达,此处直接构造超界图证伪)。
    #[test]
    fn hidx_encode_rejects_degree_above_u16() {
        let mut graph = Graph::new();
        graph.push_node(0);
        graph.set_neighbors(0, 0, (0..65_536_u32).collect());
        let error = encode(&graph, GRAPH_PARAMS).expect_err("度数超 u16 必须拒绝编码");
        assert!(matches!(error, MnemeError::Inconsistent { .. }));
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
        let small_len = encode(&chain(50), GRAPH_PARAMS).expect("encode").len();
        let large_len = encode(&chain(200), GRAPH_PARAMS).expect("encode").len();
        let decoded = decode(&encode(&chain(200), GRAPH_PARAMS).expect("encode")).expect("decode");
        assert_eq!(decoded.graph.node_count(), 200);
        // 邻接也被完整恢复(链中间的节点有左右两条边)。
        assert_eq!(decoded.graph.neighbors(100, 0).len(), 2);
        let ratio = large_len as f64 / small_len as f64;
        assert!(ratio < 6.0, "hidx 长度增长过快(疑似超线性):{ratio}");
    }

    /// 重算头部 CRC(改动头部字段后调用,否则会先被 CRC 拦下)。
    fn refresh_header_crc(bytes: &mut [u8]) {
        let crc = crc32(&bytes[0..36]);
        bytes[36..40].copy_from_slice(&crc.to_le_bytes());
    }

    /// 重算负载 CRC(改动节点表/邻接区后调用)。
    fn refresh_payload_crc(bytes: &mut [u8]) {
        let payload_end = bytes.len() - 4;
        let crc = crc32(&bytes[HEADER_LEN as usize..payload_end]);
        bytes[payload_end..payload_end + 4].copy_from_slice(&crc.to_le_bytes());
    }

    /// FC-INDEX-ERR-001:截断、头部 CRC/`header_len`/参数非法 → `Corrupted`。
    #[test]
    fn hidx_rejects_truncated_or_malformed_header() {
        let valid = encode(&sample_graph(), GRAPH_PARAMS).expect("encode");

        // 短于头部。
        let truncated = &valid[..HEADER_LEN as usize - 1];
        assert!(matches!(
            decode(truncated),
            Err(MnemeError::Corrupted { .. })
        ));

        // 头部 CRC 翻转。
        let mut bad_crc = valid.clone();
        bad_crc[36] ^= 0x01;
        assert!(matches!(
            decode(&bad_crc),
            Err(MnemeError::Corrupted { .. })
        ));

        // header_len 不符。
        let mut bad_header_len = valid.clone();
        bad_header_len[6..8].copy_from_slice(&(HEADER_LEN - 1).to_le_bytes());
        assert!(matches!(
            decode(&bad_header_len),
            Err(MnemeError::Corrupted { .. })
        ));

        // m < 2(先重算头 CRC 才能抵达参数校验)。
        let mut bad_m = valid.clone();
        bad_m[8..10].copy_from_slice(&1_u16.to_le_bytes());
        refresh_header_crc(&mut bad_m);
        assert!(matches!(decode(&bad_m), Err(MnemeError::Corrupted { .. })));

        // ml = NaN。
        let mut bad_ml = valid;
        bad_ml[16..20].copy_from_slice(&f32::NAN.to_le_bytes());
        refresh_header_crc(&mut bad_ml);
        assert!(matches!(decode(&bad_ml), Err(MnemeError::Corrupted { .. })));
    }

    /// FC-INDEX-ERR-001:长度/布局不符(节点表长度、总长、逐节点偏移)→ `Corrupted`。
    #[test]
    fn hidx_rejects_bad_layout() {
        let valid = encode(&sample_graph(), GRAPH_PARAMS).expect("encode");

        // node_table_len 与 count 不符。
        let mut bad_table_len = valid.clone();
        bad_table_len[28..32].copy_from_slice(&7_u32.to_le_bytes());
        refresh_header_crc(&mut bad_table_len);
        assert!(matches!(
            decode(&bad_table_len),
            Err(MnemeError::Corrupted { .. })
        ));

        // 尾部多余字节 → 文件总长与头部不符。
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(matches!(
            decode(&trailing),
            Err(MnemeError::Corrupted { .. })
        ));

        // 逐节点 adj_off 与真实布局不符。
        let mut bad_off = valid;
        bad_off[HEADER_LEN as usize + 1..HEADER_LEN as usize + 5]
            .copy_from_slice(&1_u32.to_le_bytes());
        refresh_payload_crc(&mut bad_off);
        assert!(matches!(
            decode(&bad_off),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    /// FC-INDEX-ERR-001:入口越界、邻居 id 越界、自环 → `Corrupted`。
    #[test]
    fn hidx_rejects_bad_neighbors() {
        let valid = encode(&sample_graph(), GRAPH_PARAMS).expect("encode");

        // 入口槽位越界(count = 3,entry_slot = 3)。
        let mut bad_entry = valid.clone();
        bad_entry[24..28].copy_from_slice(&3_u32.to_le_bytes());
        refresh_header_crc(&mut bad_entry);
        assert!(matches!(
            decode(&bad_entry),
            Err(MnemeError::Corrupted { .. })
        ));

        // 邻居 id 越界:节点 0 第 0 层的首个邻居改为 99。
        let adj_start = HEADER_LEN as usize + 3 * NODE_TABLE_ENTRY;
        let mut bad_neighbor = valid.clone();
        bad_neighbor[adj_start + 2..adj_start + 6].copy_from_slice(&99_u32.to_le_bytes());
        refresh_payload_crc(&mut bad_neighbor);
        assert!(matches!(
            decode(&bad_neighbor),
            Err(MnemeError::Corrupted { .. })
        ));

        // 自环:节点 0 第 0 层的首个邻居改为 0。
        let mut self_loop = valid;
        self_loop[adj_start + 2..adj_start + 6].copy_from_slice(&0_u32.to_le_bytes());
        refresh_payload_crc(&mut self_loop);
        assert!(matches!(
            decode(&self_loop),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    /// 合法 hidx 编码(随机层级、合法邻居、入口取最高层节点),作为变异基底。
    fn valid_hidx_strategy() -> impl Strategy<Value = Vec<u8>> {
        proptest::collection::vec(0u8..=2, 1..=8).prop_map(|levels| {
            let mut graph = Graph::new();
            for &level in &levels {
                graph.push_node(level);
            }
            let count = levels.len() as u32;
            for node in 0..count {
                for layer in 0..=levels[node as usize] as usize {
                    let neighbor = (node + 1 + layer as u32) % count;
                    if neighbor != node {
                        graph.add_neighbor(node, layer, neighbor);
                    }
                }
            }
            // 入口 = 最高层节点(`max_by_key` 同层取最后一个,任何最高层节点均合法)。
            let (entry, &level) = levels
                .iter()
                .enumerate()
                .max_by_key(|&(_, &level)| level)
                .expect("levels 非空");
            graph.entry = entry as u32;
            graph.entry_level = level;
            encode(&graph, GRAPH_PARAMS).expect("合法图必须可编码")
        })
    }

    /// 在合法编码上翻一个字节并重算对应 CRC,使变异体穿过 CRC 抵达布局/语义校验。
    fn mutated_valid_hidx_strategy() -> impl Strategy<Value = Vec<u8>> {
        (valid_hidx_strategy(), any::<usize>(), any::<u8>()).prop_map(|(mut bytes, pos, value)| {
            let index = pos % bytes.len();
            bytes[index] = value;
            if index < HEADER_LEN as usize {
                refresh_header_crc(&mut bytes);
            } else {
                refresh_payload_crc(&mut bytes);
            }
            bytes
        })
    }

    proptest! {
        /// FC-INDEX-ERR-001:任意字节不 panic;合法/变异编码必须能穿过 CRC 抵达
        /// 布局与语义校验(「接受 ⇒ 往返」),拒绝"闷声收下乱码"。
        #[test]
        fn hidx_decode_never_panics_on_arbitrary_bytes(
            bytes in prop_oneof![
                proptest::collection::vec(any::<u8>(), 0..4096),
                valid_hidx_strategy(),
                mutated_valid_hidx_strategy(),
            ]
        ) {
            let Ok(decoded) = decode(&bytes) else {
                return Ok(());
            };
            let params = GraphParams {
                m: decoded.m,
                m0: decoded.m0,
                ef_construction: decoded.ef_construction,
                ml: decoded.ml,
            };
            let re = encode(&decoded.graph, params).expect("已接受图必须可重编码");
            let again = decode(&re).expect("重编码后必须可解码");
            prop_assert_eq!(again.graph.node_count(), decoded.graph.node_count());
            for node in 0..decoded.graph.node_count() as u32 {
                for layer in 0..=decoded.graph.levels[node as usize] as usize {
                    prop_assert_eq!(
                        again.graph.neighbors(node, layer),
                        decoded.graph.neighbors(node, layer)
                    );
                }
            }
        }
    }
}
