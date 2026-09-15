//! 层搜索、贪心下降与距离打分原语。

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::core::metric::{Metric, Score};
use crate::memory::index::{QuantQuery, QuantQueryScoreInput};

use super::HnswIndex;

/// 查询向量及其预计算范数平方;打包传参以避免在层搜索接口上堆叠参数。
///
/// `quant` 为段级量化查询形式(`Some` = 本次搜索走量化粗排;设计 08 §4);
/// `bias` 只改前沿出堆顺序、不改最终打分(FC-SCORE-POST-007)。
#[derive(Clone, Copy)]
pub(crate) struct QueryRef<'a> {
    /// 查询向量。
    pub(crate) vector: &'a [f32],
    /// 查询向量范数平方(度量需要时)。
    pub(crate) norm_sq: f32,
    /// 量化粗排预计算(`None` = 精确 f32)。
    pub(crate) quant: Option<&'a QuantQuery>,
    /// 重要性偏置(`None` = 关闭;仅影响遍历顺序)。
    pub(crate) bias: Option<&'a dyn crate::memory::index::NodeBias>,
}

/// 单次层搜索的访问标记:generation 数组替代 `HashSet`,消除逐次分配与哈希。
///
/// 每线程复用一份([`VISITED`]);`begin` 推进代次故无需清零,O(1) 初始化,
/// 容量按节点数扩容。语义与 `HashSet<u32>` 等价(每个节点在一次搜索内首次访问)。
#[derive(Default)]
struct Visited {
    /// 当前代次;`marks[node] == generation` 表示本代已访问。
    generation: u32,
    /// 每节点最近访问代次。
    marks: Vec<u32>,
}

impl Visited {
    /// 开始一次新搜索:容量不足时扩容,代次回绕时清零重来。
    fn begin(&mut self, nodes: usize) {
        if self.marks.len() < nodes {
            self.marks.resize(nodes, 0);
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.marks.fill(0);
            self.generation = 1;
        }
    }

    /// 标记节点已访问;首次返回 `true`,重复返回 `false`。
    fn insert(&mut self, node: u32) -> bool {
        let mark = &mut self.marks[node as usize];
        if *mark == self.generation {
            false
        } else {
            *mark = self.generation;
            true
        }
    }
}

thread_local! {
    /// 每线程复用的搜索访问标记(`search_layer` 在构建与查询热路径上)。
    static VISITED: RefCell<Visited> = RefCell::new(Visited::default());
}

/// 在复用访问标记上执行搜索主体;重入(借用冲突)时退回一次性局部标记。
fn with_visited<R>(nodes: usize, body: impl FnOnce(&mut Visited) -> R) -> R {
    VISITED.with(|cell| match cell.try_borrow_mut() {
        Ok(mut visited) => {
            visited.begin(nodes);
            body(&mut visited)
        }
        // reason: `NodeBias` 等外部回调若在搜索中重入,借用冲突时用局部标记兜底
        // (仅多一次分配),语义不变,绝不 panic。
        Err(_) => {
            let mut visited = Visited {
                generation: 1,
                marks: vec![0; nodes],
            };
            body(&mut visited)
        }
    })
}

// 单测操作计数:统计距离计算次数(线程局部,避免测试间干扰)。
#[cfg(test)]
thread_local! {
    pub(super) static DIST_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn bump_dist_calls() {
    DIST_CALLS.with(|calls| calls.set(calls.get() + 1));
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

/// 偏置路由的固定系数(设计 10 §2.3:β 为实现内部固定 1.0)。
const BIAS_BETA: f32 = 1.0;

/// 偏置路由的前沿项:`priority` 只决定出堆顺序,`cand.key` 仍是未偏置真实键。
#[derive(Debug, Clone, Copy)]
struct BiasCand {
    priority: f32,
    cand: Cand,
}

impl PartialEq for BiasCand {
    fn eq(&self, other: &Self) -> bool {
        self.priority.total_cmp(&other.priority) == Ordering::Equal && self.cand == other.cand
    }
}

impl Eq for BiasCand {}

impl PartialOrd for BiasCand {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BiasCand {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .total_cmp(&other.priority)
            .then(self.cand.node.cmp(&other.cand.node))
    }
}

/// 层搜索的两个堆:偏置前沿(待扩展)与真实键结果集(当前最优)。
#[derive(Default)]
struct CandidateSets {
    /// 待扩展前沿(带偏置优先级;优先级只决定出堆顺序)。
    frontier: BinaryHeap<BiasCand>,
    /// 当前最优结果(小顶堆,按"越近键"取反)。
    results: BinaryHeap<std::cmp::Reverse<Cand>>,
}

impl CandidateSets {
    /// 把候选同时推入前沿与结果堆(结果堆存未偏置真实键)。
    fn push(&mut self, cand: Cand, priority: f32) {
        self.frontier.push(BiasCand { priority, cand });
        self.results.push(std::cmp::Reverse(cand));
    }

    /// 前沿当前项是否已被结果堆淘汰;仅比较"越近键",同分节点不得因 node 次序
    /// 被提前剪枝(保召回)。
    fn dominated(&self, current: Cand, ef: usize) -> bool {
        if self.results.len() < ef {
            return false;
        }
        let worst = self.results.peek().map_or(current, |rev| rev.0);
        current.key.total_cmp(&worst.key) == Ordering::Less
    }
}

/// 把原始分数转为"越大越近"的排序键。
fn close_key(metric: Metric, score: Score) -> f32 {
    match metric {
        Metric::Euclidean => -score,
        Metric::Cosine | Metric::Dot => score,
    }
}

impl HnswIndex {
    /// 以给定入口搜索一层,返回按优劣排序的 `(score, node)`(best-first)。
    ///
    /// 遍历不受过滤位图限制(死节点/历史版本可穿越以保连通),结果取舍由
    /// [`super::super::filtered`] 在返回后按 alive/过滤位图完成。`query.bias` 开启时,
    /// **前沿出堆顺序**按 `priority = key + β·bias(slot)` 偏置(β=1.0);
    /// 结果堆与最终分数仍是未偏置的真实分(`FC-SCORE-POST-007`)。
    pub(crate) fn search_layer(
        &self,
        query: QueryRef<'_>,
        start: u32,
        ef: usize,
        level: usize,
    ) -> Vec<(Score, u32)> {
        let ef = ef.max(1);
        with_visited(self.nodes.len(), |visited| {
            let mut sets = CandidateSets::default();
            if visited.insert(start) {
                let score = self.score_query(query, start);
                let cand = self.cand(score, start);
                sets.push(cand, self.bias_priority(query.bias, &cand));
            }
            while let Some(item) = sets.frontier.pop() {
                let current = item.cand;
                if sets.dominated(current, ef) {
                    break;
                }
                for &neighbor in self.graph.neighbors(current.node, level) {
                    if !visited.insert(neighbor) {
                        continue;
                    }
                    let score = self.score_query(query, neighbor);
                    let cand = self.cand(score, neighbor);
                    if sets.results.len() < ef {
                        sets.push(cand, self.bias_priority(query.bias, &cand));
                    } else if let Some(worst) = sets.results.peek().map(|rev| rev.0)
                        && cand.key.total_cmp(&worst.key) == Ordering::Greater
                    {
                        sets.results.pop();
                        sets.push(cand, self.bias_priority(query.bias, &cand));
                    }
                }
            }
            let mut out: Vec<Cand> = sets.results.into_iter().map(|rev| rev.0).collect();
            out.sort_by(|a, b| b.cmp(a));
            out.into_iter()
                .map(|cand| (cand.score, cand.node))
                .collect()
        })
    }

    /// 遍历前沿优先级:无偏置时即真实键;有偏置时加 `β·bias(全局槽位)`。
    fn bias_priority(&self, bias: Option<&dyn crate::memory::index::NodeBias>, cand: &Cand) -> f32 {
        let Some(bias) = bias else {
            return cand.key;
        };
        cand.key + BIAS_BETA * bias.bias(self.slot_of(cand.node))
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

    /// 节点-查询距离;构建期 `Hybrid` 档优先读临时 i8 码流,查询期量化段
    /// (存储副本)走粗排,其余走 f32 精确距离。
    pub(crate) fn score_query(&self, query: QueryRef<'_>, node: u32) -> Score {
        #[cfg(test)]
        bump_dist_calls();
        let target = &self.nodes[node as usize];
        if let Some(prepared) = query.quant {
            // 构建期(`Hybrid`):段内临时码流,节点顺序与 `nodes` 一致。
            if let Some(build) = self.build.as_ref()
                && let Some(codes) = build.row(node as usize)
            {
                return prepared.score(&QuantQueryScoreInput {
                    metric: self.metric,
                    query: query.vector,
                    query_norm: query.norm_sq,
                    codes,
                    target_norm: target.norm_sq,
                });
            }
            // 查询期:段存储的量化副本(设计 08 §4)。
            if let Some(copy) = self.quant.as_ref()
                && let Some(codes) = copy.rows.row(node as usize)
            {
                return prepared.score(&QuantQueryScoreInput {
                    metric: self.metric,
                    query: query.vector,
                    query_norm: query.norm_sq,
                    codes,
                    target_norm: target.norm_sq,
                });
            }
        }
        self.metric
            .score(query.vector, &target.vector, query.norm_sq, target.norm_sq)
    }

    /// 节点-节点距离。
    pub(super) fn score_pair(&self, a: u32, b: u32) -> Score {
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
}
