use crate::core::error::{MnemeError, Result};
use crate::index::graph::Graph;
use crate::persist::{FORMAT_VERSION, crc32, put_u16, put_u32};

/// hidx 魔数。
pub(crate) const MAGIC: [u8; 4] = *b"HID1";
/// 定长头部长度(字节)。
pub(crate) const HEADER_LEN: u16 = 64;
/// 单节点表条目字节数(`u8 level` + `u32 adj_off`)。
pub(crate) const NODE_TABLE_ENTRY: usize = 5;

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
