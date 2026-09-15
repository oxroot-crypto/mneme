//! `hnsw` 模块的单元测试(HNSW 建图/查询/量化副本与操作计数)。

use super::*;

use std::collections::HashSet;
use std::sync::Arc;

use crate::core::bitset::BitSet;
use crate::core::metric::Metric;
use crate::core::options::{BuildPrecision, HnswBuildParams, HnswParams, VectorFormat};
use crate::core::simd;
use crate::core::types::{RowId, SlotId};
use crate::index::graph::GraphStore;
use crate::memory::index::{IndexNode, IndexSearch, QuantCopy, VectorIndex};

use super::build::{
    BuildWithOptionsInput, build_batch_rows, build_first_batch_rows, build_threads,
};
use super::quant::{build_codes, validate_quant};
use super::search::DIST_CALLS;

fn make_nodes(count: usize, dim: usize) -> Vec<IndexNode> {
    (0..count)
        .map(|row| {
            let vector: Vec<f32> = (0..dim)
                .map(|col| (((row * 7 + col * 13) % 101) as f32) / 101.0)
                .collect();
            let norm_sq = simd::dot(&vector, &vector);
            IndexNode {
                rowid: RowId::new(row as u64),
                vector: crate::memory::lazy::VectorStorage::owned(Arc::from(
                    vector.into_boxed_slice(),
                )),
                norm_sq,
            }
        })
        .collect()
}

fn build_with(count: usize) -> HnswIndex {
    let nodes = make_nodes(count, 8);
    let params = HnswParams {
        m: 4,
        m0: 8,
        ef_construction: 32,
        ef_search: 16,
    };
    HnswIndex::build(&nodes, params, Metric::Dot)
}

fn build_calls(count: usize) -> u64 {
    DIST_CALLS.with(|calls| calls.set(0));
    let _index = build_with(count);
    DIST_CALLS.with(std::cell::Cell::get)
}

/// 断言 `FC-INDEX-INV-007` 图不变量(度数上界、无自环、邻居有效、入口 = 最高层)。
fn assert_graph_invariants(index: &HnswIndex) {
    let graph = match &index.graph {
        GraphStore::Heap(graph) => graph,
        GraphStore::Mapped(_) => panic!("构建路径必为堆图"),
    };
    assert_eq!(
        graph.levels[graph.entry as usize], graph.entry_level,
        "入口节点的存储层级与入口层级不一致"
    );
    assert_eq!(
        graph.entry_level,
        index.max_level(),
        "入口必须是全图最高层节点"
    );
    for node in 0..graph.node_count() as u32 {
        let level = graph.levels[node as usize] as usize;
        for layer in 0..=level {
            let neighbors: Vec<u32> = graph.neighbors(node, layer).to_vec();
            let bound = if layer == 0 { index.m0 } else { index.m };
            assert!(neighbors.len() <= bound, "度数超过上界");
            assert!(!neighbors.contains(&node), "存在自环");
            for neighbor in neighbors {
                assert!((neighbor as usize) < graph.node_count(), "邻居 id 越界");
            }
        }
    }
}

/// FC-INDEX-POST-012:批大小只依赖节点数与配置(小图退化串行、大图按批行数);
/// 首批受 `m0` 约束保证冷启动核心连通;线程数受配置上限约束。
#[test]
fn build_batch_rows_depends_only_on_count() {
    let defaults = HnswBuildParams::default();
    assert_eq!(build_batch_rows(0, &defaults), 1);
    assert_eq!(build_batch_rows(1, &defaults), 1);
    assert_eq!(build_batch_rows(64, &defaults), 1);
    assert_eq!(build_batch_rows(65, &defaults), 8);
    assert_eq!(build_batch_rows(1_000_000, &defaults), 8);
    let custom = HnswBuildParams {
        serial_rows: 100,
        batch_rows: 32,
        ..HnswBuildParams::default()
    };
    assert_eq!(build_batch_rows(100, &custom), 1);
    assert_eq!(build_batch_rows(101, &custom), 32);
    assert_eq!(build_first_batch_rows(32, 32), 32);
    assert_eq!(build_first_batch_rows(32, 4), 4);
    assert_eq!(build_first_batch_rows(1, 32), 1);
    assert_eq!(build_first_batch_rows(16, 0), 1);
    assert_eq!(build_threads(64, 8, 8), 8, "线程数不超过批行数");
    assert_eq!(build_threads(64, 32, 3), 3, "线程数受配置上限约束");
    assert_eq!(build_threads(2, 32, 8), 2, "显式线程数不放大");
}

/// 无周期重复的大规模确定性节点(可达性回归用;`make_nodes` 的取模周期在
/// 2000 行上会退化出大量相同向量,不适合该用例)。
fn make_unique_nodes(count: usize, dim: usize) -> Vec<IndexNode> {
    (0..count)
        .map(|row| {
            let vector: Vec<f32> = (0..dim)
                .map(|col| (((row * 37 + col * 101) % 9_973) as f32) / 9_973.0)
                .collect();
            let norm_sq = simd::dot(&vector, &vector);
            IndexNode {
                rowid: RowId::new(row as u64),
                vector: crate::memory::lazy::VectorStorage::owned(Arc::from(
                    vector.into_boxed_slice(),
                )),
                norm_sq,
            }
        })
        .collect()
}

/// FC-INDEX-POST-012:批内并行构建后,从入口沿出边可达全部节点
/// (`ef→∞` 收敛与召回门槛的图结构前提)。
#[test]
fn parallel_build_keeps_graph_reachable_from_entry() {
    let nodes = make_unique_nodes(2000, 8);
    let params = HnswParams {
        m: 4,
        m0: 8,
        ef_construction: 32,
        ef_search: 16,
    };
    let index = HnswIndex::build_with_options(BuildWithOptionsInput {
        nodes: &nodes,
        params,
        metric: Metric::Dot,
        precision: BuildPrecision::Hybrid,
        parallelism: 4,
    })
    .expect("并行构建");
    let graph = match &index.graph {
        GraphStore::Heap(graph) => graph,
        GraphStore::Mapped(_) => panic!("构建路径必为堆图"),
    };
    let mut seen = vec![false; graph.node_count()];
    let mut stack = vec![graph.entry];
    seen[graph.entry as usize] = true;
    while let Some(node) = stack.pop() {
        let level = graph.levels[node as usize] as usize;
        for layer in 0..=level {
            for &neighbor in graph.neighbors(node, layer) {
                if !seen[neighbor as usize] {
                    seen[neighbor as usize] = true;
                    stack.push(neighbor);
                }
            }
        }
    }
    let unreachable = seen.iter().filter(|&&value| !value).count();
    assert_eq!(unreachable, 0, "并行构建后存在从入口不可达的节点");
}

/// FC-INDEX-POST-012:同输入下构建结果与线程数无关(逐字节相同 hidx)。
#[test]
fn parallel_build_is_thread_count_independent() {
    let nodes = make_nodes(600, 8);
    let params = HnswParams {
        m: 4,
        m0: 8,
        ef_construction: 32,
        ef_search: 16,
    };
    let single = HnswIndex::build_with_options(BuildWithOptionsInput {
        nodes: &nodes,
        params,
        metric: Metric::Dot,
        precision: BuildPrecision::Hybrid,
        parallelism: 1,
    })
    .expect("单线程构建");
    let parallel = HnswIndex::build_with_options(BuildWithOptionsInput {
        nodes: &nodes,
        params,
        metric: Metric::Dot,
        precision: BuildPrecision::Hybrid,
        parallelism: 4,
    })
    .expect("四线程构建");
    assert_eq!(
        single.serialize().expect("serialize"),
        parallel.serialize().expect("serialize"),
        "批内并行结果必须与线程数无关"
    );
    assert_graph_invariants(&parallel);
}

/// FC-INDEX-POST-010:`Hybrid` 档构建确定(同输入逐字节同图)且图不变量成立。
#[test]
fn hybrid_build_is_deterministic_and_valid() {
    let nodes = make_nodes(300, 8);
    let params = HnswParams {
        m: 4,
        m0: 8,
        ef_construction: 32,
        ef_search: 16,
    };
    let first =
        HnswIndex::build_with_precision(&nodes, params, Metric::Dot, BuildPrecision::Hybrid)
            .expect("hybrid build");
    let second =
        HnswIndex::build_with_precision(&nodes, params, Metric::Dot, BuildPrecision::Hybrid)
            .expect("hybrid build");
    assert_eq!(
        first.serialize().expect("serialize"),
        second.serialize().expect("serialize"),
        "同输入必须产生同一图"
    );
    assert_graph_invariants(&first);
    assert_graph_invariants(&second);
}

/// FC-INDEX-POST-010:档位语义——`F32` 档不生成临时码流;`Hybrid` 档生成
/// 段级码流且逐维解码误差 ≤ `Δ/2`(与 `FC-QUANT-POST-001` 同源)。
#[test]
fn build_codes_follow_precision() {
    let nodes = make_nodes(64, 8);
    let dimension = 8;
    assert!(
        build_codes(BuildPrecision::F32, &nodes, nodes.len(), dimension)
            .expect("F32 档")
            .is_none(),
        "F32 档不得生成临时码流"
    );
    let codes = build_codes(BuildPrecision::Hybrid, &nodes, nodes.len(), dimension)
        .expect("Hybrid 档")
        .expect("Hybrid 档必须生成临时码流");
    assert_eq!(codes.codes.len(), nodes.len() * dimension);
    for (index, node) in nodes.iter().enumerate() {
        let row = codes.row(index).expect("码流行存在");
        let restored = crate::quant::scalar_i8::decode_row(row, &codes.params);
        for (dim, (&original, &approx)) in node.vector.iter().zip(&restored).enumerate() {
            let bound = codes.params.delta(dim) / 2.0;
            assert!(
                (original - approx).abs() <= bound + 1e-6,
                "节点 {index} 第 {dim} 维误差超过上界"
            );
        }
    }
}

/// FC-INDEX-POST-010:连续数据上 `Hybrid` 档确实使用近似距离(图不同于 `F32`),
/// 证伪"档位未生效、实际仍走 f32"。
#[test]
fn hybrid_uses_approximate_distances_on_continuous_data() {
    let nodes = make_nodes(500, 8);
    let params = HnswParams {
        m: 4,
        m0: 8,
        ef_construction: 32,
        ef_search: 16,
    };
    let exact = HnswIndex::build_with_precision(&nodes, params, Metric::Dot, BuildPrecision::F32)
        .expect("exact build");
    let hybrid =
        HnswIndex::build_with_precision(&nodes, params, Metric::Dot, BuildPrecision::Hybrid)
            .expect("hybrid build");
    assert_ne!(
        exact.serialize().expect("serialize"),
        hybrid.serialize().expect("serialize"),
        "Hybrid 档必须实际改变建图距离"
    );
}

/// 把 hidx 字节包成句柄视图(测试用;L2 打开路径由段句柄提供)。
fn hidx_span(bytes: &[u8]) -> crate::memory::lazy::ByteSpan {
    crate::memory::lazy::ByteSpan::whole(crate::memory::lazy::OwnedBytes::new(bytes.to_vec()))
        .expect("hidx span")
}

/// FC-INDEX-INV-007:每层度数 ≤ M0/M、无自环、邻居 id 有效;
/// 入口节点必须是全图最高层节点(构建路径同口径)。
#[test]
fn graph_degree_and_self_loop_invariants() {
    let index = build_with(300);
    assert_eq!(index.node_count(), 300);
    assert_graph_invariants(&index);
}

/// FC-INDEX-CPLX-001(操作计数:构建距离计算随节点数近似线性,远离二次)。
#[test]
fn build_distance_calls_scale_linearly() {
    let c200 = build_calls(200);
    let c500 = build_calls(500);
    let c1200 = build_calls(1200);
    assert!(c200 > 0);
    let r1 = c500 as f64 / c200 as f64;
    let r2 = c1200 as f64 / c500 as f64;
    // 规模比 2.5/2.4 倍;阈值 4.5 留出批内构建的图演化噪声,仍可证伪二次
    // (O(N²) 会给出 ~6.25x)。
    assert!(r1 < 4.5, "200→500 增长过快(疑似二次):{r1}");
    assert!(r2 < 4.5, "500→1200 增长过快(疑似二次):{r2}");
    // 每节点成本不应随规模显著上升(排除超线性)。
    let per_small = c200 as f64 / 200.0;
    let per_large = c1200 as f64 / 1200.0;
    assert!(per_large / per_small < 3.0, "每节点成本随规模增长:超线性");
}

/// 在给定 `ef` 下执行一次查询并返回距离计算次数。
fn search_calls(index: &HnswIndex, query: &[f32], alive: &BitSet, ef: usize) -> u64 {
    DIST_CALLS.with(|calls| calls.set(0));
    let _ = index.search(&IndexSearch {
        query,
        query_norm: simd::dot(query, query),
        ef,
        k: 10,
        alive,
        filter: None,
        post_threshold: 0.1,
        brute_threshold: 0.001,
        use_quant: false,
        bias: None,
    });
    DIST_CALLS.with(std::cell::Cell::get)
}

/// FC-INDEX-CPLX-002(操作计数:查询距离计算随 `ef` 增长、远小于 N,无全扫)。
#[test]
fn search_distance_calls_bounded_by_ef() {
    let nodes = make_nodes(3000, 8);
    let params = HnswParams {
        m: 4,
        m0: 8,
        ef_construction: 32,
        ef_search: 16,
    };
    let index = HnswIndex::build(&nodes, params, Metric::Dot);
    let query = make_nodes(1, 8).remove(0).vector;
    let mut alive = BitSet::default();
    for node in 0..index.node_count() {
        alive.set(node);
    }
    let calls16 = search_calls(&index, &query, &alive, 16);
    let calls128 = search_calls(&index, &query, &alive, 128);
    assert!(calls16 > 0);
    // 距离调用随 ef 单调不减(图连通分量被探尽后会持平,故不强制严格增长);
    // 关键是远小于 N,证明查询没有退化为全扫。
    assert!(calls128 >= calls16, "距离计算随 ef 单调不减");
    assert!(
        calls128 < nodes.len() as u64 / 2,
        "ef=128 时距离计算疑似全扫:{calls128}"
    );
}

/// FC-INDEX-CPLX-003:层高随 N 单调不减且保持对数级(远离线性层数)。
#[test]
fn level_height_grows_logarithmically() {
    let small = HnswIndex::build(
        &make_nodes(500, 4),
        HnswParams {
            m: 16,
            m0: 32,
            ef_construction: 32,
            ef_search: 16,
        },
        Metric::Dot,
    );
    let large = HnswIndex::build(
        &make_nodes(4000, 4),
        HnswParams {
            m: 16,
            m0: 32,
            ef_construction: 32,
            ef_search: 16,
        },
        Metric::Dot,
    );
    assert!(large.max_level() >= small.max_level(), "层高随 N 下降");
    assert!((1..=8).contains(&small.max_level()), "层高异常");
    assert!((1..=8).contains(&large.max_level()), "层高异常");
}

/// FC-INDEX-POST-005:查询结果全部落在 alive 位图内,且候选足够时满额返回、
/// 与 alive 内暴力结果集合一致(空返/少返同样被证伪)。
#[test]
fn search_results_respect_alive_bitmap() {
    let nodes = make_nodes(64, 8);
    let index = HnswIndex::build(&nodes, HnswParams::default(), Metric::Dot);
    let mut alive = BitSet::default();
    for node in 0..32usize {
        alive.set(node);
    }
    let query = make_nodes(1, 8).remove(0).vector;
    let query_norm = simd::dot(&query, &query);
    let top = index.search(&IndexSearch {
        query: &query,
        query_norm,
        ef: 64,
        k: 16,
        alive: &alive,
        filter: None,
        post_threshold: 0.1,
        brute_threshold: 0.001,
        use_quant: false,
        bias: None,
    });
    let hits = top.into_sorted_vec();
    assert_eq!(
        hits.len(),
        16,
        "alive 内候选足够时必须满额返回,不得被死节点挤占"
    );
    let got: HashSet<u64> = hits.iter().map(|(rowid, _)| rowid.get()).collect();
    // alive 内暴力 oracle:ef=64 覆盖全图 64 节点,结果须与暴力 top-16 集合相等。
    let mut scored: Vec<(f32, u64)> = (0..32usize)
        .map(|node| {
            let score =
                Metric::Dot.score(&query, &nodes[node].vector, query_norm, nodes[node].norm_sq);
            (score, nodes[node].rowid.get())
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let want: HashSet<u64> = scored
        .into_iter()
        .take(16)
        .map(|(_, rowid)| rowid)
        .collect();
    assert_eq!(got, want, "alive 位图内结果应与暴力一致");
}

/// FC-INDEX-ERR-001:hidx 节点数与恢复槽位数不一致 → `Corrupted`,绝不静默错配。
#[test]
fn load_rejects_node_count_mismatch() {
    let nodes = make_nodes(3, 8);
    let index = HnswIndex::build(&nodes, HnswParams::default(), Metric::Dot);
    let bytes = index.serialize().expect("serialize");
    let slot_of: Vec<SlotId> = (0..3).map(SlotId::new).collect();
    // 图节点数 > 恢复槽位数。
    let error = HnswIndex::load(HnswLoadInput {
        span: &hidx_span(&bytes),
        nodes: &nodes[..2],
        slot_of: &slot_of[..2],
        metric: Metric::Dot,
        quant: None,
    })
    .err()
    .expect("节点数不一致必须拒绝载入");
    assert!(matches!(
        error,
        crate::core::error::MnemeError::Corrupted { .. }
    ));
    // `slot_of` 长度不一致同样拒绝。
    let error = HnswIndex::load(HnswLoadInput {
        span: &hidx_span(&bytes),
        nodes: &nodes,
        slot_of: &slot_of[..2],
        metric: Metric::Dot,
        quant: None,
    })
    .err()
    .expect("槽位数不一致必须拒绝载入");
    assert!(matches!(
        error,
        crate::core::error::MnemeError::Corrupted { .. }
    ));
    // 边界对照:完全一致时可载入。
    assert!(
        HnswIndex::load(HnswLoadInput {
            span: &hidx_span(&bytes),
            nodes: &nodes,
            slot_of: &slot_of,
            metric: Metric::Dot,
            quant: None,
        })
        .is_ok()
    );
}

/// 构建确定性:同一输入两次构建产生逐字节相同的 hidx(设计 05 §4.4:固定种子串行构建,便于复现)。
#[test]
fn build_is_deterministic() {
    let nodes = make_nodes(256, 8);
    let params = HnswParams {
        m: 4,
        m0: 8,
        ef_construction: 32,
        ef_search: 16,
    };
    let first = HnswIndex::build(&nodes, params, Metric::Dot)
        .serialize()
        .expect("serialize");
    let second = HnswIndex::build(&nodes, params, Metric::Dot)
        .serialize()
        .expect("serialize");
    assert_eq!(first, second, "同一输入必须产生同一图");
}

/// 退化边界:空图与单节点图的构建/编解码往返(FC-INDEX-POST-007 的退化边界)与
/// 查询不 panic、不越界。
#[test]
fn empty_and_single_node_graphs_are_supported() {
    // 空图:构建、序列化、载入均成立;查询返回空。
    let empty = HnswIndex::build(&[], HnswParams::default(), Metric::Dot);
    assert_eq!(empty.node_count(), 0);
    let bytes = empty.serialize().expect("serialize empty");
    assert!(
        HnswIndex::load(HnswLoadInput {
            span: &hidx_span(&bytes),
            nodes: &[],
            slot_of: &[],
            metric: Metric::Dot,
            quant: None,
        })
        .is_ok()
    );
    let alive = BitSet::default();
    let top = empty.search(&IndexSearch {
        query: &[1.0, 0.0],
        query_norm: 1.0,
        ef: 8,
        k: 4,
        alive: &alive,
        filter: None,
        post_threshold: 0.1,
        brute_threshold: 0.001,
        use_quant: false,
        bias: None,
    });
    assert!(top.into_sorted_vec().is_empty(), "空图不得返回任何命中");

    // 单节点图:查询能返回该节点。
    let nodes = make_nodes(1, 8);
    let single = HnswIndex::build(&nodes, HnswParams::default(), Metric::Dot);
    let mut alive = BitSet::default();
    alive.set(0);
    let query = nodes[0].vector.clone();
    let hits = single
        .search(&IndexSearch {
            query: &query,
            query_norm: simd::dot(&query, &query),
            ef: 8,
            k: 4,
            alive: &alive,
            filter: None,
            post_threshold: 0.1,
            brute_threshold: 0.001,
            use_quant: false,
            bias: None,
        })
        .into_sorted_vec();
    assert_eq!(hits.len(), 1, "单节点图必须返回唯一节点");
    assert_eq!(hits[0].0.get(), 0);
}

/// FC-INDEX-ERR-001 / FC-QUANT-ERR-003(量化副本校验):格式、行数、单行
/// 长度、i8 参数表长度与 f16 维度不符 → `Corrupted`,绝不静默按错误码流打分。
#[test]
fn validate_quant_rejects_malformed_copies() {
    for (copy, count, dimension) in malformed_quant_copies() {
        assert!(
            validate_quant(&Some(copy), count, dimension).is_err(),
            "应拒绝畸形副本: count={count} dimension={dimension}"
        );
    }
    // 正例:合法 i8 副本通过校验(防过度拒绝)。
    assert!(validate_quant(&Some(legal_i8_copy()), 1, 2).is_ok());
}

/// 合法 i8 副本(2 维、1 行、参数表 2d 项)。
fn legal_i8_copy() -> QuantCopy {
    QuantCopy {
        format: VectorFormat::I8Rescored,
        params: vec![0.0, 1.0, 0.0, 1.0],
        rows: crate::memory::lazy::LazyRows::from_owned(vec![0_u8; 2], 2, 1).expect("合法行区"),
    }
}

/// 各畸形副本及调用维度的用例集。
fn malformed_quant_copies() -> Vec<(QuantCopy, usize, usize)> {
    let copy =
        |format: VectorFormat, rows: crate::memory::lazy::LazyRows, params: Vec<f32>| QuantCopy {
            format,
            params,
            rows,
        };
    let rows = |bytes: usize, stride: usize, count: usize| {
        crate::memory::lazy::LazyRows::from_owned(vec![0_u8; bytes], stride, count)
            .expect("测试行区构造")
    };
    vec![
        // F32 不允许携带副本。
        (copy(VectorFormat::F32, rows(2, 2, 1), Vec::new()), 1, 2),
        // 行数与节点数不符。
        (
            copy(
                VectorFormat::I8Rescored,
                rows(2, 2, 1),
                vec![0.0, 1.0, 0.0, 1.0],
            ),
            2,
            2,
        ),
        // 单行长度与维度不符(行距 1 ≠ 维度 2)。
        (
            copy(
                VectorFormat::I8Rescored,
                rows(1, 1, 1),
                vec![0.0, 1.0, 0.0, 1.0],
            ),
            1,
            2,
        ),
        // i8 参数表长度与维度不符(应 2d = 4)。
        (
            copy(VectorFormat::I8Rescored, rows(2, 2, 1), vec![0.0, 1.0]),
            1,
            2,
        ),
        // f16 副本自推维度(2)与索引节点维度(1)不符。
        (copy(VectorFormat::F16, rows(4, 4, 1), Vec::new()), 1, 1),
    ]
}
