use super::*;
use crate::core::error::MnemeError;
use crate::index::graph::Graph;
use crate::persist::{FORMAT_VERSION, crc32};
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

/// FC-INDEX-ERR-001:更高/更低版本 → `UnsupportedVersion`(I18)。
#[test]
fn hidx_rejects_version_mismatch() {
    for version in [0x0100_u16, crate::persist::FORMAT_VERSION - 1] {
        let mut bytes = encode(&sample_graph(), GRAPH_PARAMS).expect("encode");
        bytes[4..6].copy_from_slice(&version.to_le_bytes());
        let crc = crc32(&bytes[0..36]);
        bytes[36..40].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            decode(&bytes),
            Err(MnemeError::UnsupportedVersion { .. })
        ));
    }
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
    bad_off[HEADER_LEN as usize + 1..HEADER_LEN as usize + 5].copy_from_slice(&1_u32.to_le_bytes());
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
