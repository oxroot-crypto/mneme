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
use crate::memory::index::{IndexNode, IndexSearch, MAX_INDEX_DEGREE, VectorIndex};

use super::filtered;
use super::graph::Graph;
use super::hidx;

/// 构建期确定性种子(固定 -> 同输入同图,便于测试与回归)。
const BUILD_SEED: u64 = 0x4D4E_454D_4500_0001;
/// 层级骰子上限(防 `-ln(u)` 极端值导致层级爆炸)。
const MAX_ROLL_LEVEL: usize = 31;

/// 查询向量及其预计算范数平方;打包传参以避免在层搜索接口上堆叠参数。
#[derive(Debug, Clone, Copy)]
pub(crate) struct QueryRef<'a> {
    /// 查询向量。
    pub(crate) vector: &'a [f32],
    /// 查询向量范数平方(度量需要时)。
    pub(crate) norm_sq: f32,
}

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

/// SplitMix64 增量常数(Steele et al., 2014;用于确定性层级骰子,无外部依赖)。
const SPLITMIX_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
/// SplitMix64 第一次混合乘数。
const SPLITMIX_MIX1: u64 = 0xBF58_476D_1CE4_E5B9;
/// SplitMix64 第二次混合乘数。
const SPLITMIX_MIX2: u64 = 0x94D0_49BB_1331_11EB;

/// 确定性 SplitMix64 伪随机数发生器(无外部依赖)。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(SPLITMIX_GAMMA);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(SPLITMIX_MIX1);
        z = (z ^ (z >> 27)).wrapping_mul(SPLITMIX_MIX2);
        z ^ (z >> 31)
    }

    /// 均匀分布于 `(0, 1]`。
    fn next_unit(&mut self) -> f32 {
        // 取高 24 位并加一,落在 `(0, 1]`(避免 `ln(0)`)。
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
    /// 由节点构建 HNSW 图(节点 id = 全局槽位 `0..nodes.len()` 的恒等映射)。
    ///
    /// 仅测试与解析证明使用;生产路径经 [`build_with_slots`](Self::build_with_slots)。
    #[cfg(test)]
    pub(crate) fn build(nodes: &[IndexNode], params: HnswParams, metric: Metric) -> Self {
        let slot_of: Vec<SlotId> = (0..nodes.len()).map(|id| SlotId::new(id as u32)).collect();
        Self::build_with_slots(nodes, &slot_of, params, metric)
    }

    /// 由节点与显式槽位映射构建 HNSW 图(增量段用;`slot_of` 与 `nodes` 等长)。
    pub(crate) fn build_with_slots(
        nodes: &[IndexNode],
        slot_of: &[SlotId],
        params: HnswParams,
        metric: Metric,
    ) -> Self {
        // 调用方(增量段装配)始终同源构造两个等长切片;取短边只是防御性兜底,
        // 保证 release 下绝不因内部装配失误越界 panic(FC-GLOBAL-ERR-001 口径)。
        debug_assert_eq!(
            nodes.len(),
            slot_of.len(),
            "节点数与槽位映射必须等长(FC-INDEX-INV-007)"
        );
        let count = nodes.len().min(slot_of.len());
        let m = (params.m.max(2) as usize).min(MAX_INDEX_DEGREE as usize);
        let m0 = (params.m0 as usize).max(m).min(MAX_INDEX_DEGREE as usize);
        let ef_construction = params.ef_construction.max(1) as usize;
        let ml = 1.0 / (m as f32).ln();
        let mut index = Self {
            nodes: Vec::with_capacity(count),
            slot_of: Vec::with_capacity(count),
            graph: Graph::new(),
            m,
            m0,
            ml,
            ef_construction,
            metric,
        };
        let mut rng = Rng::new(BUILD_SEED);
        for position in 0..count {
            let level = roll_level(&mut rng, ml);
            index.nodes.push(nodes[position].clone());
            index.slot_of.push(slot_of[position]);
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

    /// 建库时锁定的距离度量(查询与索引用同一度量,避免调用方重复传入而不一致)。
    pub(crate) fn metric(&self) -> Metric {
        self.metric
    }

    /// 节点 id 对应的稳定 `RowId`。
    pub(crate) fn rowid(&self, node: u32) -> RowId {
        self.nodes[node as usize].rowid
    }

    /// 入口节点 id。
    pub(crate) fn entry_node(&self) -> u32 {
        self.graph.entry
    }

    /// 以给定入口搜索一层,返回按优劣排序的 `(score, node)`(best-first)。
    ///
    /// 遍历不受过滤位图限制(死节点/历史版本可穿越以保连通),结果取舍由
    /// [`super::filtered`] 在返回后按 alive/过滤位图完成。
    pub(crate) fn search_layer(
        &self,
        query: QueryRef<'_>,
        start: u32,
        ef: usize,
        level: usize,
    ) -> Vec<(Score, u32)> {
        let ef = ef.max(1);
        let mut visited: HashSet<u32> = HashSet::new();
        let mut frontier: BinaryHeap<Cand> = BinaryHeap::new();
        let mut results: BinaryHeap<std::cmp::Reverse<Cand>> = BinaryHeap::new();
        if visited.insert(start) {
            let score = self.score_query(query, start);
            let cand = self.cand(score, start);
            frontier.push(cand);
            results.push(std::cmp::Reverse(cand));
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
                let score = self.score_query(query, neighbor);
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
    pub(crate) fn greedy_query(&self, query: QueryRef<'_>, mut node: u32, level: usize) -> u32 {
        let mut best = self.score_query(query, node);
        loop {
            let mut improved = false;
            for &neighbor in self.graph.neighbors(node, level) {
                let score = self.score_query(query, neighbor);
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
    pub(crate) fn score_query(&self, query: QueryRef<'_>, node: u32) -> Score {
        #[cfg(test)]
        bump_dist_calls();
        let target = &self.nodes[node as usize];
        self.metric
            .score(query.vector, &target.vector, query.norm_sq, target.norm_sq)
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
        // 克隆 `Arc` 以免 `query` 借用 `self.nodes` 与后续 `&mut self` 冲突。
        let vector = Arc::clone(&self.nodes[node as usize].vector);
        let query = QueryRef {
            vector: &vector,
            norm_sq: self.nodes[node as usize].norm_sq,
        };
        let target = level as usize;
        let top = self.graph.entry_level as usize;
        let mut entry = self.graph.entry;
        if target < top {
            for layer in ((target + 1)..=top).rev() {
                entry = self.greedy_query(query, entry, layer);
            }
        }
        let start = target.min(top);
        for layer in (0..=start).rev() {
            let candidates = self.search_layer(query, entry, self.ef_construction, layer);
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

    fn serialize(&self) -> Result<Vec<u8>> {
        hidx::encode(
            &self.graph,
            hidx::GraphParams {
                m: self.m as u16,
                m0: self.m0 as u16,
                ef_construction: self.ef_construction as u16,
                ml: self.ml,
            },
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

    /// FC-INDEX-INV-007:每层度数 ≤ M0/M、无自环、邻居 id 有效;
    /// 入口节点必须是全图最高层节点(构建路径同口径)。
    #[test]
    fn graph_degree_and_self_loop_invariants() {
        let index = build_with(300);
        assert_eq!(index.node_count(), 300);
        assert_eq!(
            index.graph.levels[index.graph.entry as usize], index.graph.entry_level,
            "入口节点的存储层级与入口层级不一致"
        );
        assert_eq!(
            index.graph.entry_level,
            index.max_level(),
            "入口必须是全图最高层节点"
        );
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
        let error = HnswIndex::load(&bytes, &nodes[..2], &slot_of[..2], Metric::Dot)
            .err()
            .expect("节点数不一致必须拒绝载入");
        assert!(matches!(
            error,
            crate::core::error::MnemeError::Corrupted { .. }
        ));
        // `slot_of` 长度不一致同样拒绝。
        let error = HnswIndex::load(&bytes, &nodes, &slot_of[..2], Metric::Dot)
            .err()
            .expect("槽位数不一致必须拒绝载入");
        assert!(matches!(
            error,
            crate::core::error::MnemeError::Corrupted { .. }
        ));
        // 边界对照:完全一致时可载入。
        assert!(HnswIndex::load(&bytes, &nodes, &slot_of, Metric::Dot).is_ok());
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
        assert!(HnswIndex::load(&bytes, &[], &[], Metric::Dot).is_ok());
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
            })
            .into_sorted_vec();
        assert_eq!(hits.len(), 1, "单节点图必须返回唯一节点");
        assert_eq!(hits[0].0.get(), 0);
    }
}
