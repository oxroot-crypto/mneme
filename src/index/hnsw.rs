//! 自研 HNSW 索引(`hnsw.rs`,设计 05 §3–§6)。
//!
//! 分层可导航小世界图:节点 id = 段内槽位下标,向量借用 [`IndexNode`] 的 `Arc`。
//! 构建期用确定性种子分配层级,使同一输入产生同一图(测试可复现);查询经
//! [`super::filtered`] 走过滤三档。

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;

use crate::core::error::Result;
use crate::core::heap::TopK;
use crate::core::metric::{Metric, Score};
use crate::core::options::HnswParams;
use crate::core::types::{RowId, SlotId};
use crate::memory::index::{IndexNode, IndexSearch, VectorIndex};

use super::filtered;
use super::graph::Graph;
use super::hidx;

/// 构建期确定性种子(固定 -> 同输入同图,便于测试与回归)。
const BUILD_SEED: u64 = 0x4D4E_454D_4500_0001;
/// 层级骰子上限(防 `-ln(u)` 极端值导致层级爆炸)。
const MAX_ROLL_LEVEL: usize = 31;

// 单测操作计数:统计距离计算次数(线程局部,避免测试间干扰)。
#[cfg(test)]
thread_local! {
    static DIST_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn bump_dist_calls() {
    DIST_CALLS.with(|calls| calls.set(calls.get() + 1));
}

/// 建图节点与图的只读访问(供 `filtered` 与上层查询)。
pub(crate) struct HnswIndex {
    nodes: Vec<IndexNode>,
    slot_of: Vec<SlotId>,
    graph: Graph,
    m: usize,
    m0: usize,
    ml: f32,
    ef_construction: usize,
    metric: Metric,
}

/// 分数与节点组成的可比较候选(按"越近键越大"排序)。
#[derive(Debug, Clone, Copy)]
struct Cand {
    key: f32,
    score: Score,
    node: u32,
}

impl PartialEq for Cand {
    fn eq(&self, other: &Self) -> bool {
        self.key.total_cmp(&other.key) == Ordering::Equal && self.node == other.node
    }
}

impl Eq for Cand {}

impl PartialOrd for Cand {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Cand {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key
            .total_cmp(&other.key)
            .then(self.node.cmp(&other.node))
    }
}

/// 把原始分数转为"越大越近"的排序键。
fn close_key(metric: Metric, score: Score) -> f32 {
    match metric {
        Metric::Euclidean => -score,
        Metric::Cosine | Metric::Dot => score,
    }
}

/// 确定性 SplitMix64 伪随机数发生器(无外部依赖)。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 均匀分布于 `(0, 1]`。
    fn next_unit(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32 + 1.0) * (1.0 / (1_u32 << 24) as f32)
    }
}

/// 掷层级:`floor(-ln(u) * ml)`,即 `P(level ≥ l) = M^{-l}`(设计 05 §3.2)。
fn roll_level(rng: &mut Rng, ml: f32) -> u8 {
    // `next_unit()` ∈ (0,1],`ml > 0`,故 `-ln(u)*ml ≥ 0`。
    let level = (-rng.next_unit().ln() * ml).floor();
    if level as usize > MAX_ROLL_LEVEL {
        MAX_ROLL_LEVEL as u8
    } else {
        level as u8
    }
}

impl HnswIndex {
    /// 由节点构建 HNSW 图。
    pub(crate) fn build(nodes: &[IndexNode], params: HnswParams, metric: Metric) -> Self {
        let m = params.m.max(2) as usize;
        let m0 = params.m0.max(params.m.max(2)) as usize;
        let ef_construction = params.ef_construction.max(1) as usize;
        let ml = 1.0 / (m as f32).ln();
        let mut index = Self {
            nodes: Vec::with_capacity(nodes.len()),
            slot_of: Vec::with_capacity(nodes.len()),
            graph: Graph::new(),
            m,
            m0,
            ml,
            ef_construction,
            metric,
        };
        let mut rng = Rng::new(BUILD_SEED);
        for (position, node) in nodes.iter().enumerate() {
            let level = roll_level(&mut rng, ml);
            index.nodes.push(node.clone());
            index.slot_of.push(SlotId::new(position as u32));
            index.graph.push_node(level);
            index.link_node(position as u32, level);
        }
        index
    }

    /// 由 hidx 字节载入图(节点顺序与 `nodes`/`slot_of` 对齐)。
    ///
    /// 图参数以 hidx 头部为准;`metric` 取自库配置(建库即锁定)。
    ///
    /// # Errors
    /// hidx 解析失败或节点数不一致时返回结构化错误。
    pub(crate) fn load(
        bytes: &[u8],
        nodes: &[IndexNode],
        slot_of: &[SlotId],
        metric: Metric,
    ) -> Result<Self> {
        let decoded = hidx::decode(bytes)?;
        if decoded.graph.node_count() != nodes.len() || slot_of.len() != nodes.len() {
            return Err(crate::core::error::MnemeError::Corrupted {
                segment: None,
                reason: "hidx: 节点数与恢复槽位数不一致".to_string(),
            });
        }
        Ok(Self {
            nodes: nodes.to_vec(),
            slot_of: slot_of.to_vec(),
            graph: decoded.graph,
            m: decoded.m.max(2) as usize,
            m0: decoded.m0.max(2) as usize,
            ml: decoded.ml,
            ef_construction: decoded.ef_construction.max(1) as usize,
            metric,
        })
    }

    /// 节点 id 对应的全局槽位。
    pub(crate) fn slot_of(&self, node: u32) -> SlotId {
        self.slot_of[node as usize]
    }

    /// 节点 id 对应的稳定 `RowId`。
    pub(crate) fn rowid(&self, node: u32) -> RowId {
        self.nodes[node as usize].rowid
    }

    /// 入口节点 id。
    pub(crate) fn entry_node(&self) -> u32 {
        self.graph.entry
    }

    /// 以给定谓词搜索一层,返回按优劣排序的 `(score, node)`(best-first)。
    ///
    /// `traverse(node) == false` 的节点被标记已访问但不展开(过滤三档的约束遍历)。
    pub(crate) fn search_layer(
        &self,
        query: &[f32],
        query_norm: f32,
        entries: &[u32],
        ef: usize,
        level: usize,
        traverse: &dyn Fn(u32) -> bool,
    ) -> Vec<(Score, u32)> {
        let ef = ef.max(1);
        let mut visited: HashSet<u32> = HashSet::new();
        let mut frontier: BinaryHeap<Cand> = BinaryHeap::new();
        let mut results: BinaryHeap<std::cmp::Reverse<Cand>> = BinaryHeap::new();
        for &entry in entries.iter().take(ef) {
            if visited.insert(entry) {
                let score = self.score_query(query, query_norm, entry);
                let cand = self.cand(score, entry);
                frontier.push(cand);
                results.push(std::cmp::Reverse(cand));
            }
        }
        while let Some(current) = frontier.pop() {
            if results.len() >= ef {
                let worst = results.peek().map_or(current, |rev| rev.0);
                // 仅比较"越近键":同分节点不得因 node 次序被提前剪枝(保召回)。
                if current.key.total_cmp(&worst.key) == Ordering::Less {
                    break;
                }
            }
            for &neighbor in self.graph.neighbors(current.node, level) {
                if !visited.insert(neighbor) {
                    continue;
                }
                if !traverse(neighbor) {
                    continue;
                }
                let score = self.score_query(query, query_norm, neighbor);
                let cand = self.cand(score, neighbor);
                if results.len() < ef {
                    frontier.push(cand);
                    results.push(std::cmp::Reverse(cand));
                } else if let Some(worst) = results.peek().map(|rev| rev.0)
                    && cand.key.total_cmp(&worst.key) == Ordering::Greater
                {
                    results.pop();
                    results.push(std::cmp::Reverse(cand));
                    frontier.push(cand);
                }
            }
        }
        let mut out: Vec<Cand> = results.into_iter().map(|rev| rev.0).collect();
        out.sort_by(|a, b| b.cmp(a));
        out.into_iter()
            .map(|cand| (cand.score, cand.node))
            .collect()
    }

    /// 在给定层做贪心下降,返回距查询最优的节点(best-first 单步)。
    pub(crate) fn greedy_query(
        &self,
        query: &[f32],
        query_norm: f32,
        mut node: u32,
        level: usize,
    ) -> u32 {
        let mut best = self.score_query(query, query_norm, node);
        loop {
            let mut improved = false;
            for &neighbor in self.graph.neighbors(node, level) {
                let score = self.score_query(query, query_norm, neighbor);
                if self.metric.better(score, best) {
                    node = neighbor;
                    best = score;
                    improved = true;
                }
            }
            if !improved {
                break;
            }
        }
        node
    }

    /// 节点-查询距离。
    pub(crate) fn score_query(&self, query: &[f32], query_norm: f32, node: u32) -> Score {
        #[cfg(test)]
        bump_dist_calls();
        let target = &self.nodes[node as usize];
        self.metric
            .score(query, &target.vector, query_norm, target.norm_sq)
    }

    /// 节点-节点距离。
    fn score_pair(&self, a: u32, b: u32) -> Score {
        #[cfg(test)]
        bump_dist_calls();
        let left = &self.nodes[a as usize];
        let right = &self.nodes[b as usize];
        self.metric
            .score(&left.vector, &right.vector, left.norm_sq, right.norm_sq)
    }

    /// 构造排序候选。
    fn cand(&self, score: Score, node: u32) -> Cand {
        Cand {
            key: close_key(self.metric, score),
            score,
            node,
        }
    }

    /// 插入一个新节点:从入口逐层下降 → 目标层搜索 → 启发式连边与修剪。
    fn link_node(&mut self, node: u32, level: u8) {
        if node == 0 {
            self.graph.entry = 0;
            self.graph.entry_level = level;
            return;
        }
        let query = Arc::clone(&self.nodes[node as usize].vector);
        let query_norm = self.nodes[node as usize].norm_sq;
        let target = level as usize;
        let top = self.graph.entry_level as usize;
        let mut entry = self.graph.entry;
        if target < top {
            for layer in ((target + 1)..=top).rev() {
                entry = self.greedy_query(&query, query_norm, entry, layer);
            }
        }
        let always = |_node: u32| true;
        let start = target.min(top);
        for layer in (0..=start).rev() {
            let candidates = self.search_layer(
                &query,
                query_norm,
                &[entry],
                self.ef_construction,
                layer,
                &always,
            );
            let max_conn = if layer == 0 { self.m0 } else { self.m };
            let selected = self.select_neighbors(node, &candidates, max_conn);
            for &neighbor in &selected {
                self.graph.add_neighbor(node, layer, neighbor);
                self.graph.add_neighbor(neighbor, layer, node);
                if self.graph.degree(neighbor, layer) > max_conn {
                    self.prune(neighbor, layer, max_conn);
                }
            }
            if let Some(&(_, best)) = candidates.first() {
                entry = best;
            }
        }
        if target > top {
            self.graph.entry = node;
            self.graph.entry_level = level;
        }
    }

    /// 启发式选邻(设计 05 §4.3):优先保留提供新方向的候选,不足则回填。
    fn select_neighbors(
        &self,
        owner: u32,
        candidates: &[(Score, u32)],
        max_conn: usize,
    ) -> Vec<u32> {
        let mut selected: Vec<u32> = Vec::with_capacity(max_conn);
        for &(score_to_owner, candidate) in candidates {
            if selected.len() >= max_conn {
                break;
            }
            if candidate == owner {
                continue;
            }
            let mut keep = true;
            for &chosen in &selected {
                let score_to_chosen = self.score_pair(candidate, chosen);
                // 候选离 owner 比离已选邻居更近 -> 提供新方向。
                if !self.metric.better(score_to_owner, score_to_chosen) {
                    keep = false;
                    break;
                }
            }
            if keep {
                selected.push(candidate);
            }
        }
        if selected.len() < max_conn {
            for &(_, candidate) in candidates {
                if selected.len() >= max_conn {
                    break;
                }
                if candidate != owner && !selected.contains(&candidate) {
                    selected.push(candidate);
                }
            }
        }
        selected
    }

    /// 修剪超员节点的邻集到 `max_conn`。
    fn prune(&mut self, node: u32, level: usize, max_conn: usize) {
        let current: Vec<u32> = self.graph.neighbors(node, level).to_vec();
        let mut scored: Vec<(Score, u32)> = current
            .iter()
            .map(|&neighbor| (self.score_pair(node, neighbor), neighbor))
            .collect();
        scored.sort_by(|a, b| {
            if self.metric.better(a.0, b.0) {
                Ordering::Less
            } else if self.metric.better(b.0, a.0) {
                Ordering::Greater
            } else {
                a.1.cmp(&b.1)
            }
        });
        let selected = self.select_neighbors(node, &scored, max_conn);
        self.graph.set_neighbors(node, level, selected);
    }
}

impl VectorIndex for HnswIndex {
    fn node_count(&self) -> usize {
        self.nodes.len()
    }

    fn max_level(&self) -> u8 {
        self.graph.levels.iter().copied().max().unwrap_or(0)
    }

    fn entry(&self) -> (SlotId, u8) {
        if self.nodes.is_empty() {
            return (SlotId::new(0), 0);
        }
        (
            self.slot_of[self.graph.entry as usize],
            self.graph.entry_level,
        )
    }

    fn serialize(&self) -> Vec<u8> {
        hidx::encode(
            &self.graph,
            self.m as u16,
            self.m0 as u16,
            self.ef_construction as u16,
            self.ml,
        )
    }

    fn search(&self, params: &IndexSearch<'_>) -> TopK<(RowId, SlotId)> {
        filtered::search(self, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bitset::BitSet;
    use crate::core::simd;

    fn make_nodes(count: usize, dim: usize) -> Vec<IndexNode> {
        (0..count)
            .map(|row| {
                let vector: Vec<f32> = (0..dim)
                    .map(|col| (((row * 7 + col * 13) % 101) as f32) / 101.0)
                    .collect();
                let norm_sq = simd::dot(&vector, &vector);
                IndexNode {
                    rowid: RowId::new(row as u64),
                    vector: Arc::from(vector.into_boxed_slice()),
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

    /// FC-INDEX-INV-007:每层度数 ≤ M0/M、无自环、邻居 id 有效。
    #[test]
    fn graph_degree_and_self_loop_invariants() {
        let index = build_with(300);
        assert_eq!(index.node_count(), 300);
        for node in 0..index.graph.node_count() as u32 {
            let level = index.graph.levels[node as usize] as usize;
            for layer in 0..=level {
                let neighbors: Vec<u32> = index.graph.neighbors(node, layer).to_vec();
                let bound = if layer == 0 { index.m0 } else { index.m };
                assert!(neighbors.len() <= bound, "度数超过上界");
                assert!(!neighbors.contains(&node), "存在自环");
                for neighbor in neighbors {
                    assert!(
                        (neighbor as usize) < index.graph.node_count(),
                        "邻居 id 越界"
                    );
                }
            }
        }
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
        assert!(r1 < 4.0, "200→500 增长过快(疑似二次):{r1}");
        assert!(r2 < 4.0, "500→1200 增长过快(疑似二次):{r2}");
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
            metric: Metric::Dot,
            alive,
            filter: None,
            post_threshold: 0.1,
            brute_threshold: 0.001,
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
        assert!(calls128 >= calls16, "距离计算未随 ef 增长");
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

    /// FC-INDEX-POST-005:查询结果全部落在 alive 位图内。
    #[test]
    fn search_results_respect_alive_bitmap() {
        let nodes = make_nodes(64, 8);
        let index = HnswIndex::build(&nodes, HnswParams::default(), Metric::Dot);
        let mut alive = BitSet::default();
        for node in 0..32u32 {
            alive.set(node as usize);
        }
        let query = make_nodes(1, 8).remove(0).vector;
        let top = index.search(&IndexSearch {
            query: &query,
            query_norm: simd::dot(&query, &query),
            ef: 64,
            k: 16,
            metric: Metric::Dot,
            alive: &alive,
            filter: None,
            post_threshold: 0.1,
            brute_threshold: 0.001,
        });
        for (_, slot) in top.into_sorted_vec() {
            assert!(slot.get() < 32, "结果越出 alive 位图:{}", slot.get());
        }
    }
}
