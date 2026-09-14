//! 自研 HNSW 索引(`hnsw.rs`,设计 05 §3–§6)。
//!
//! 分层可导航小世界图:节点 id = 段内槽位下标,向量借用 [`IndexNode`] 的 `Arc`。
//! 构建期用确定性种子分配层级,使同一输入产生同一图(测试可复现);查询经
//! [`super::filtered`] 走过滤三档。

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
#[cfg(test)]
use std::collections::HashSet;
use std::sync::Arc;

use crate::core::error::Result;
use crate::core::heap::TopK;
use crate::core::metric::{Metric, Score};
use crate::core::options::{BuildPrecision, HnswBuildParams, HnswParams, VectorFormat};
use crate::core::types::{RowId, SlotId};
use crate::memory::index::{
    IndexNode, IndexSearch, MAX_INDEX_DEGREE, QuantCopy, QuantQuery, VectorIndex,
};

use super::filtered;
use super::graph::{Graph, GraphStore};
use super::hidx;

/// 构建期确定性种子(固定 -> 同输入同图,便于测试与回归)。
const BUILD_SEED: u64 = 0x4D4E_454D_4500_0001;
/// 层级骰子上限(防 `-ln(u)` 极端值导致层级爆炸)。
const MAX_ROLL_LEVEL: usize = 31;

/// 由节点数与建图参数推导批行数(小图退化串行;只依赖节点数与配置,与线程数无关)。
fn build_batch_rows(count: usize, build: &HnswBuildParams) -> usize {
    if count <= build.serial_rows {
        1
    } else {
        build.batch_rows
    }
}

/// 首批行数:受 `m0` 约束(首批节点都只连得到 0 号节点,超出 `m0` 会被修剪掉
/// 入边导致不可达),保证初始核心图连通。
fn build_first_batch_rows(batch_rows: usize, m0: usize) -> usize {
    batch_rows.min(m0.max(1)).max(1)
}

/// 实际建图线程数:`0` 按可用核数,再受批大小与配置硬上限约束。
fn build_threads(parallelism: usize, batch_rows: usize, threads_max: usize) -> usize {
    let threads = if parallelism == 0 {
        std::thread::available_parallelism().map_or(1, |count| count.get())
    } else {
        parallelism
    };
    threads.clamp(1, threads_max.max(1)).min(batch_rows.max(1))
}

/// 单节点的建图计划(批内并行计算、批间串行应用;`FC-INDEX-POST-012`)。
struct LinkPlan {
    /// 各层选中的邻居 `(层号, 邻居节点)`;按层号降序(与构建下降顺序一致)。
    layers: Vec<(usize, Vec<u32>)>,
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

// 单测操作计数:统计距离计算次数(线程局部,避免测试间干扰)。
#[cfg(test)]
thread_local! {
    static DIST_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn bump_dist_calls() {
    DIST_CALLS.with(|calls| calls.set(calls.get() + 1));
}

/// `Hybrid` 建图档的段内临时 i8 码流(不落盘,构建结束释放)。
///
/// 节点顺序与 `nodes` 一致;参数/编码复用 `FC-QUANT-POST-001` 的段级逐维口径,
/// 误差上界同为 `Δ/2`(`FC-INDEX-POST-010`)。
struct BuildCodes {
    /// 段级逐维量化参数。
    params: crate::quant::scalar_i8::I8Params,
    /// 连续码流(`count × dimension` 字节)。
    codes: Vec<u8>,
}

impl BuildCodes {
    /// 第 `node` 个节点的码流切片(节点序号越界或维数为 0 返回 `None`)。
    fn row(&self, node: usize) -> Option<&[u8]> {
        let dimension = self.params.dimension();
        if dimension == 0 {
            return None;
        }
        let start = node.checked_mul(dimension)?;
        self.codes.get(start..start.checked_add(dimension)?)
    }
}

/// 建图节点与图的只读访问(供 `filtered` 与上层查询)。
pub(crate) struct HnswIndex {
    nodes: Vec<IndexNode>,
    slot_of: Vec<SlotId>,
    /// 图存储:构建期为堆图,hidx 载入期为惰性映射图(FC-PERSIST-INV-021)。
    graph: GraphStore,
    m: usize,
    m0: usize,
    ml: f32,
    ef_construction: usize,
    metric: Metric,
    /// 段的量化副本(节点顺序与 `nodes` 对齐);`None` = 纯 f32。
    quant: Option<QuantCopy>,
    /// i8 副本的段级参数解析结果(载入/构建时一次,查询期复用,免每查询重解析)。
    i8_params: Option<crate::quant::scalar_i8::I8Params>,
    /// `Hybrid` 建图档的临时码流;`F32` 档与载入路径恒为 `None`。
    build: Option<BuildCodes>,
    /// 启发式选邻的「新方向」比较上限(构建期由 `Tuning` 派生;载入路径为默认值)。
    compare_cap: usize,
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

/// 校验量化副本行数/单行长度/维度与图节点一致。
///
/// # Errors
/// 副本格式为 `F32`、行数或单行长度不符、i8 参数表长度与维度不符,或 f16
/// 副本自推维度与索引节点维度不符时返回
/// [`MnemeError::Corrupted`](crate::core::error::MnemeError::Corrupted)。
fn validate_quant(quant: &Option<QuantCopy>, count: usize, dimension: usize) -> Result<()> {
    let Some(copy) = quant else {
        return Ok(());
    };
    let corrupt = |reason: &str| crate::core::error::MnemeError::Corrupted {
        segment: None,
        reason: format!("hnsw: {reason}"),
    };
    if copy.format == VectorFormat::F32 || copy.rows.len() != count {
        return Err(corrupt("量化副本格式/行数与索引节点不符"));
    }
    let stride = copy.code_stride();
    if copy.rows.stride() != stride {
        return Err(corrupt("量化副本单行长度与维度不符"));
    }
    match copy.format {
        VectorFormat::I8Rescored if copy.params.len() != dimension * 2 => {
            Err(corrupt("i8 参数表长度与维度不符"))
        }
        VectorFormat::F16 if copy.dimension() != dimension => {
            Err(corrupt("f16 副本维度与索引节点不符"))
        }
        _ => Ok(()),
    }
}

/// 按建图精度档位生成段内临时 i8 码流(`F32` 档或空段返回 `None`)。
///
/// 参数/编码复用 `FC-QUANT-POST-001` 的段级逐维口径;码流只供构建期遍历距离
/// 计算,不写入任何文件(FC-INDEX-POST-010)。
///
/// # Errors
/// 段内向量维度不一致或含非有限分量时返回结构化错误,绝不静默跳过量化。
fn build_codes(
    precision: BuildPrecision,
    nodes: &[IndexNode],
    count: usize,
    dimension: usize,
) -> Result<Option<BuildCodes>> {
    if precision == BuildPrecision::F32 || count == 0 || dimension == 0 {
        return Ok(None);
    }
    let vectors: Vec<&[f32]> = nodes[..count].iter().map(|node| &node.vector[..]).collect();
    let params = crate::quant::scalar_i8::build_params(&vectors, dimension)?;
    let mut codes = Vec::with_capacity(count.saturating_mul(dimension));
    for vector in &vectors {
        crate::quant::scalar_i8::encode_row_into(&mut codes, vector, &params);
    }
    Ok(Some(BuildCodes { params, codes }))
}

/// 解析并缓存 i8 副本的段级参数(仅 `I8Rescored` 副本;畸形参数退回 `None`)。
///
/// 参数与图节点同寿命且不可变,查询期只需按查询向量预计算权重;解析失败与
/// [`quantize_query`](HnswIndex::quantize_query) 的旧口径一致:本次查询退 f32 精确路径。
fn cached_i8_params(
    quant: &Option<QuantCopy>,
    dimension: usize,
) -> Option<crate::quant::scalar_i8::I8Params> {
    let copy = quant.as_ref()?;
    if copy.format != VectorFormat::I8Rescored {
        return None;
    }
    crate::quant::scalar_i8::I8Params::from_table(&copy.params, dimension).ok()
}

impl HnswIndex {
    /// 由节点构建 HNSW 图(节点 id = 全局槽位 `0..nodes.len()` 的恒等映射)。
    ///
    /// 仅测试与解析证明使用;生产路径经 [`build_with_slots`](Self::build_with_slots)。
    #[cfg(test)]
    pub(crate) fn build(nodes: &[IndexNode], params: HnswParams, metric: Metric) -> Self {
        Self::build_with_precision(nodes, params, metric, BuildPrecision::default())
            .expect("测试输入恒满足建图前置")
    }

    /// 测试辅助:指定精度档位构建(恒等槽位映射;并行度按可用核数)。
    #[cfg(test)]
    fn build_with_precision(
        nodes: &[IndexNode],
        params: HnswParams,
        metric: Metric,
        precision: BuildPrecision,
    ) -> Result<Self> {
        Self::build_with_options(nodes, params, metric, precision, 0)
    }

    /// 测试辅助:显式指定精度档位与并行度构建(恒等槽位映射;其余建图参数用默认)。
    #[cfg(test)]
    fn build_with_options(
        nodes: &[IndexNode],
        params: HnswParams,
        metric: Metric,
        precision: BuildPrecision,
        parallelism: usize,
    ) -> Result<Self> {
        let slot_of: Vec<SlotId> = (0..nodes.len()).map(|id| SlotId::new(id as u32)).collect();
        Self::build_with_slots(
            nodes,
            &slot_of,
            params,
            metric,
            None,
            precision,
            HnswBuildParams {
                parallelism,
                ..HnswBuildParams::default()
            },
        )
    }

    /// 由节点与显式槽位映射构建 HNSW 图(增量段用;`slot_of` 与 `nodes` 等长)。
    ///
    /// `quant` 只服务查询期粗排打分;`precision` 为建图距离精度档位(设计 05 §4.4、
    /// `FC-INDEX-POST-010`):`Hybrid` 档在段内生成临时 i8 码流供遍历近似、
    /// 选邻前用 f32 精排,临时码流不落盘且构建结束即释放。
    ///
    /// 构建按 [`build_batch_rows`] 分批:批内节点基于**批开始图快照**并行计算
    /// 选邻计划(只读),批间按节点序串行应用(连边/修剪/入口推进);批大小只依赖
    /// 节点数、结果与线程数无关(同输入同配置同图,`FC-INDEX-POST-012`)。
    ///
    /// # Errors
    /// `Hybrid` 档段内量化(维度一致性与有限性)失败、或建图线程 panic 时返回
    /// 结构化错误,绝不静默降级为其它档位。
    pub(crate) fn build_with_slots(
        nodes: &[IndexNode],
        slot_of: &[SlotId],
        params: HnswParams,
        metric: Metric,
        quant: Option<QuantCopy>,
        precision: BuildPrecision,
        build: HnswBuildParams,
    ) -> Result<Self> {
        // 调用方(增量段装配)始终同源构造两个等长切片;取短边只是防御性兜底,
        // 保证 release 下绝不因内部装配失误越界 panic(FC-GLOBAL-ERR-001 口径)。
        debug_assert_eq!(
            nodes.len(),
            slot_of.len(),
            "节点数与槽位映射必须等长(FC-INDEX-INV-007)"
        );
        let count = nodes.len().min(slot_of.len());
        let dimension = nodes.first().map_or(0, |node| node.vector.len());
        debug_assert!(validate_quant(&quant, count, dimension).is_ok());
        let m = (params.m.max(2) as usize).min(MAX_INDEX_DEGREE as usize);
        let m0 = (params.m0 as usize).max(m).min(MAX_INDEX_DEGREE as usize);
        let ef_construction = params.ef_construction.max(1) as usize;
        let ml = 1.0 / (m as f32).ln();
        let i8_params = cached_i8_params(&quant, dimension);
        let codes = build_codes(precision, nodes, count, dimension)?;
        let mut index = Self {
            nodes: Vec::with_capacity(count),
            slot_of: Vec::with_capacity(count),
            graph: GraphStore::Heap(Graph::new()),
            m,
            m0,
            ml,
            ef_construction,
            metric,
            quant,
            i8_params,
            build: codes,
            compare_cap: build.compare_cap.max(1),
        };
        // 阶段 0:预分配节点与层级(确定性 RNG 顺序;此刻图无边)。
        let mut rng = Rng::new(BUILD_SEED);
        let mut levels = Vec::with_capacity(count);
        for position in 0..count {
            let level = roll_level(&mut rng, ml);
            levels.push(level);
            index.nodes.push(nodes[position].clone());
            index.slot_of.push(slot_of[position]);
            // reason: 构建路径的图恒为 `Heap`(载入路径不经此函数);`expect` 可证明不可达。
            index
                .graph
                .heap_mut()
                .expect("构建路径恒为堆图")
                .push_node(level);
        }
        // 阶段 1/2:批内并行计算计划,批间串行应用。首批受 `m0` 约束
        // (冷启动核心的入边不被集中修剪,保证从入口可达)。
        let batch_rows = build_batch_rows(count, &build);
        let first_batch = build_first_batch_rows(batch_rows, m0);
        let threads = build_threads(build.parallelism, batch_rows, build.threads_max);
        let mut start = 0;
        while start < count {
            let size = if start == 0 { first_batch } else { batch_rows };
            let end = (start + size).min(count);
            let plans = index.plan_batch(start, end, &levels, threads)?;
            index.apply_batch(start, &plans, &levels, threads)?;
            start = end;
        }
        // 阶段 3:可达性修复(批内互不可见可能留下极少量孤立)。
        index.repair_unreachable();
        // 临时码流只在构建期生存(不落盘、不进查询路径)。
        index.build = None;
        Ok(index)
    }

    /// 并行计算 `[start, end)` 的建图计划(只读图快照;结果与线程数无关)。
    ///
    /// 动态游标分派:每个位置的计划只依赖批开始快照,分派顺序只影响耗时。
    ///
    /// # Errors
    /// 计划计算失败(档位量化不一致)或构建线程 panic 时返回结构化错误。
    fn plan_batch(
        &self,
        start: usize,
        end: usize,
        levels: &[u8],
        threads: usize,
    ) -> Result<Vec<LinkPlan>> {
        let batch = end - start;
        let workers = threads.min(batch).max(1);
        if workers <= 1 {
            return (start..end)
                .map(|position| self.plan_node(position as u32, levels[position]))
                .collect();
        }
        let cursor = std::sync::atomic::AtomicUsize::new(0);
        let mut slots: Vec<Option<LinkPlan>> = (0..batch).map(|_| None).collect();
        std::thread::scope(|scope| -> Result<()> {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    scope.spawn(|| -> Result<Vec<(usize, LinkPlan)>> {
                        let mut produced = Vec::new();
                        loop {
                            let offset = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if offset >= batch {
                                break;
                            }
                            let position = start + offset;
                            produced
                                .push((offset, self.plan_node(position as u32, levels[position])?));
                        }
                        Ok(produced)
                    })
                })
                .collect();
            for handle in handles {
                let produced =
                    handle
                        .join()
                        .map_err(|_| crate::core::error::MnemeError::Inconsistent {
                            reason: "HNSW 建图线程 panic",
                        })??;
                for (offset, plan) in produced {
                    slots[offset] = Some(plan);
                }
            }
            Ok(())
        })?;
        slots
            .into_iter()
            .map(|slot| {
                slot.ok_or(crate::core::error::MnemeError::Inconsistent {
                    reason: "HNSW 建图计划缺失",
                })
            })
            .collect()
    }

    /// 在批开始图快照上计算单节点各层的选邻计划(只读;可多线程并行)。
    ///
    /// 与串行插入的唯一语义差异是**批内互不可见**:同一批的节点只看得到批开始
    /// 时的图;批间按节点序串行应用,下一批可见。批大小只依赖节点数。
    ///
    /// # Errors
    /// `Hybrid` 档查询侧 i8 预计算失败(维度/有限性不一致)时返回结构化错误。
    fn plan_node(&self, node: u32, level: u8) -> Result<LinkPlan> {
        // 克隆 `Arc` 以免 `query` 借用 `self.nodes` 与后续只读访问冲突。
        let vector = Arc::clone(&self.nodes[node as usize].vector);
        // `Hybrid` 档:查询侧按段级参数预计算 i8 权重,遍历距离走码流
        // (FC-INDEX-POST-010);`F32` 档 `quant = None` 走精确距离。
        let prepared = match self.build.as_ref() {
            Some(build) => Some(crate::quant::scalar_i8::Query::new(&vector, &build.params)?),
            None => None,
        };
        let quant = prepared.map(QuantQuery::I8);
        let query = QueryRef {
            vector: &vector,
            norm_sq: self.nodes[node as usize].norm_sq,
            quant: quant.as_ref(),
            bias: None,
        };
        let target = level as usize;
        let top = self.graph.entry_level() as usize;
        let mut entry = self.graph.entry();
        if target < top {
            for layer in ((target + 1)..=top).rev() {
                entry = self.greedy_query(query, entry, layer);
            }
        }
        let start = target.min(top);
        let mut layers = Vec::with_capacity(start + 1);
        for layer in (0..=start).rev() {
            let candidates = self.search_layer(query, entry, self.ef_construction, layer);
            let candidates = if self.build.is_some() {
                self.refine_candidates(node, candidates)
            } else {
                candidates
            };
            let max_conn = if layer == 0 { self.m0 } else { self.m };
            let selected = self.select_neighbors(node, &candidates, max_conn);
            entry = candidates.first().map_or(entry, |&(_, best)| best);
            layers.push((layer, selected));
        }
        Ok(LinkPlan { layers })
    }

    /// 串行应用一批计划:先全部加边(无距离计算),再把超员节点的修剪计算
    /// **按批并行求值**、按 `(节点, 层)` 序串行写回。
    ///
    /// 修剪只读批末图快照、各改各的邻接表,故结果与线程数无关(确定性不变);
    /// 相比逐节点边加边剪,批末统一修剪的输入含本批全部新边,修剪更彻底。
    ///
    /// # Errors
    /// 修剪计算线程 panic 时返回结构化错误,绝不把 panic 抛给调用方。
    fn apply_batch(
        &mut self,
        start: usize,
        plans: &[LinkPlan],
        levels: &[u8],
        threads: usize,
    ) -> Result<()> {
        let mut touched: Vec<(u32, usize)> = Vec::new();
        for (offset, plan) in plans.iter().enumerate() {
            let position = start + offset;
            self.apply_links(position as u32, levels[position], plan, &mut touched);
        }
        touched.sort_unstable();
        touched.dedup();
        touched.retain(|&(node, layer)| {
            let max_conn = if layer == 0 { self.m0 } else { self.m };
            self.graph.degree(node, layer) > max_conn
        });
        let pruned = self.compute_prune_batch(&touched, threads)?;
        for ((node, layer), selected) in touched.into_iter().zip(pruned) {
            // reason: 构建路径的图恒为 `Heap`(载入路径不经此函数)。
            if let Some(graph) = self.graph.heap_mut() {
                graph.set_neighbors(node, layer, selected);
            }
        }
        Ok(())
    }

    /// 串行加边(无距离计算):双向连边、记录被触达的 `(节点, 层)`、必要时推进入口。
    fn apply_links(
        &mut self,
        node: u32,
        level: u8,
        plan: &LinkPlan,
        touched: &mut Vec<(u32, usize)>,
    ) {
        if node == 0 {
            // reason: 构建路径的图恒为 `Heap`,不可达分支显式忽略。
            if let Some(graph) = self.graph.heap_mut() {
                graph.entry = 0;
                graph.entry_level = level;
            }
            return;
        }
        for (layer, selected) in &plan.layers {
            // reason: 构建路径的图恒为 `Heap`(载入路径不经此函数)。
            let graph = self.graph.heap_mut().expect("构建路径恒为堆图");
            for &neighbor in selected {
                graph.add_neighbor(node, *layer, neighbor);
                graph.add_neighbor(neighbor, *layer, node);
            }
            touched.push((node, *layer));
            for &neighbor in selected {
                touched.push((neighbor, *layer));
            }
        }
        if level as usize > self.graph.entry_level() as usize {
            // reason: 构建路径的图恒为 `Heap`(载入路径不经此函数)。
            if let Some(graph) = self.graph.heap_mut() {
                graph.entry = node;
                graph.entry_level = level;
            }
        }
    }

    /// 并行计算一批超员节点的修剪后邻接(只读图快照;结果与线程数无关)。
    ///
    /// # Errors
    /// 修剪计算线程 panic 时返回结构化错误。
    fn compute_prune_batch(
        &self,
        targets: &[(u32, usize)],
        threads: usize,
    ) -> Result<Vec<Vec<u32>>> {
        let count = targets.len();
        let workers = threads.min(count).max(1);
        if workers <= 1 {
            return Ok(targets
                .iter()
                .map(|&(node, layer)| self.compute_pruned(node, layer))
                .collect());
        }
        let cursor = std::sync::atomic::AtomicUsize::new(0);
        let mut slots: Vec<Option<Vec<u32>>> = (0..count).map(|_| None).collect();
        std::thread::scope(|scope| -> Result<()> {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    scope.spawn(|| -> Vec<(usize, Vec<u32>)> {
                        let mut produced = Vec::new();
                        loop {
                            let index = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if index >= count {
                                break;
                            }
                            let (node, layer) = targets[index];
                            produced.push((index, self.compute_pruned(node, layer)));
                        }
                        produced
                    })
                })
                .collect();
            for handle in handles {
                let produced =
                    handle
                        .join()
                        .map_err(|_| crate::core::error::MnemeError::Inconsistent {
                            reason: "HNSW 修剪线程 panic",
                        })?;
                for (index, selected) in produced {
                    slots[index] = Some(selected);
                }
            }
            Ok(())
        })?;
        slots
            .into_iter()
            .map(|slot| {
                slot.ok_or(crate::core::error::MnemeError::Inconsistent {
                    reason: "HNSW 修剪结果缺失",
                })
            })
            .collect()
    }

    /// 计算 `node` 在 `layer` 层修剪后的邻接(只读;不写图)。
    fn compute_pruned(&self, node: u32, layer: usize) -> Vec<u32> {
        let max_conn = if layer == 0 { self.m0 } else { self.m };
        let current: Vec<u32> = self.graph.neighbors(node, layer).to_vec();
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
        self.select_neighbors(node, &scored, max_conn)
    }

    /// 构建后可达性修复:把从入口沿出边不可达的节点重连到可达集合。
    ///
    /// 批内互不可见 + 邻边集中修剪可能留下极少量不可达节点(破坏 `ef→∞` 收敛的
    /// 图结构前提);本步在全部修剪完成后执行,按不可达节点的升序构造可达链
    /// `entry → u₀ → u₁ → …`:每条链边的宿主只被修改一次、并在保护式修剪下保留,
    /// 建立后不再被任何操作触碰,可达性严格成立。
    fn repair_unreachable(&mut self) {
        // reason: 构建路径的图恒为 `Heap`(载入路径不经此函数)。
        let Some(graph) = self.graph.heap_mut() else {
            return;
        };
        let m0 = self.m0;
        let count = graph.node_count();
        if count == 0 {
            return;
        }
        let entry = graph.entry;
        let mut seen = vec![false; count];
        let mut stack = vec![entry];
        seen[entry as usize] = true;
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
        let unreachable: Vec<u32> = (0..count as u32)
            .filter(|&node| !seen[node as usize])
            .collect();
        if unreachable.is_empty() {
            return;
        }
        // 链式修复:首个不可达节点连到 entry,其后每个连到**上一个修复节点**,
        // 形成 `entry → u₀ → u₁ → …` 的可达链。每个宿主只被修改一次(修复其下一个
        // 节点时),边在**保护式修剪**下建立后不再被任何操作触碰,可达性严格成立。
        let mut previous: Option<u32> = None;
        for node in unreachable {
            let host = previous.unwrap_or(entry);
            if let Some(graph) = self.graph.heap_mut() {
                graph.add_neighbor(host, 0, node);
            }
            self.prune_protecting(host, 0, node);
            if let Some(graph) = self.graph.heap_mut()
                && graph.neighbors(node, 0).len() < m0
            {
                graph.add_neighbor(node, 0, host);
            }
            seen[node as usize] = true;
            previous = Some(node);
        }
    }

    /// 由 hidx 句柄载入图(节点顺序与 `nodes`/`slot_of` 对齐)。
    ///
    /// 只解析头部与 `node_table`;邻接字节随句柄按需解码(FC-PERSIST-INV-021)。
    /// 图参数以 hidx 头部为准;`metric` 取自库配置(建库即锁定);`quant`
    /// 为从 vsec qvec 区还原的段级副本。
    ///
    /// # Errors
    /// hidx 解析失败或节点数/量化副本不一致时返回结构化错误。
    pub(crate) fn load(
        span: &crate::memory::lazy::ByteSpan,
        nodes: &[IndexNode],
        slot_of: &[SlotId],
        metric: Metric,
        quant: Option<QuantCopy>,
    ) -> Result<Self> {
        let mapped = hidx::open(span)?;
        if mapped.node_count() != nodes.len() || slot_of.len() != nodes.len() {
            return Err(crate::core::error::MnemeError::Corrupted {
                segment: None,
                reason: "hidx: 节点数与恢复槽位数不一致".to_string(),
            });
        }
        let dimension = nodes.first().map_or(0, |node| node.vector.len());
        validate_quant(&quant, nodes.len(), dimension)?;
        let params = mapped.params();
        let i8_params = cached_i8_params(&quant, dimension);
        Ok(Self {
            nodes: nodes.to_vec(),
            slot_of: slot_of.to_vec(),
            graph: GraphStore::Mapped(mapped),
            m: params.m.max(2) as usize,
            m0: params.m0.max(2) as usize,
            ml: params.ml,
            ef_construction: params.ef_construction.max(1) as usize,
            metric,
            quant,
            i8_params,
            // 载入路径不构建(档位只影响构建期距离,hidx 不含档位元数据)。
            build: None,
            compare_cap: HnswBuildParams::default().compare_cap,
        })
    }

    /// 为本次查询预计算段级量化形式;无副本或维度不符时返回 `None`(退 f32)。
    pub(crate) fn quantize_query(&self, query: &[f32]) -> Option<QuantQuery> {
        let copy = self.quant.as_ref()?;
        match copy.format {
            VectorFormat::F32 => None,
            VectorFormat::I8Rescored => {
                let params = self.i8_params.as_ref()?;
                crate::quant::scalar_i8::Query::new(query, params)
                    .ok()
                    .map(QuantQuery::I8)
            }
            VectorFormat::F16 => {
                #[cfg(feature = "quant-f16")]
                {
                    Some(QuantQuery::F16)
                }
                #[cfg(not(feature = "quant-f16"))]
                {
                    None
                }
            }
        }
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
        self.graph.entry()
    }

    /// 以给定入口搜索一层,返回按优劣排序的 `(score, node)`(best-first)。
    ///
    /// 遍历不受过滤位图限制(死节点/历史版本可穿越以保连通),结果取舍由
    /// [`super::filtered`] 在返回后按 alive/过滤位图完成。`query.bias` 开启时,
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
            let mut frontier: BinaryHeap<BiasCand> = BinaryHeap::new();
            let mut results: BinaryHeap<std::cmp::Reverse<Cand>> = BinaryHeap::new();
            if visited.insert(start) {
                let score = self.score_query(query, start);
                let cand = self.cand(score, start);
                frontier.push(BiasCand {
                    priority: self.bias_priority(query.bias, &cand),
                    cand,
                });
                results.push(std::cmp::Reverse(cand));
            }
            while let Some(item) = frontier.pop() {
                let current = item.cand;
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
                        frontier.push(BiasCand {
                            priority: self.bias_priority(query.bias, &cand),
                            cand,
                        });
                        results.push(std::cmp::Reverse(cand));
                    } else if let Some(worst) = results.peek().map(|rev| rev.0)
                        && cand.key.total_cmp(&worst.key) == Ordering::Greater
                    {
                        results.pop();
                        results.push(std::cmp::Reverse(cand));
                        frontier.push(BiasCand {
                            priority: self.bias_priority(query.bias, &cand),
                            cand,
                        });
                    }
                }
            }
            let mut out: Vec<Cand> = results.into_iter().map(|rev| rev.0).collect();
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
                return prepared.score(
                    self.metric,
                    query.vector,
                    query.norm_sq,
                    codes,
                    target.norm_sq,
                );
            }
            // 查询期:段存储的量化副本(设计 08 §4)。
            if let Some(copy) = self.quant.as_ref()
                && let Some(codes) = copy.rows.row(node as usize)
            {
                return prepared.score(
                    self.metric,
                    query.vector,
                    query.norm_sq,
                    codes,
                    target.norm_sq,
                );
            }
        }
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

    /// `Hybrid` 档:按 f32 原向量重算候选与 owner 的距离并按精确分重排。
    ///
    /// 遍历候选来自 i8 近似分,重排保证 `select_neighbors`/`prune` 的输入
    /// 按精确分有序(同分按节点序号,与图构建的全序口径一致)。
    fn refine_candidates(&self, owner: u32, candidates: Vec<(Score, u32)>) -> Vec<(Score, u32)> {
        let owner_node = &self.nodes[owner as usize];
        let mut refined: Vec<(Score, u32)> = candidates
            .into_iter()
            .map(|(_, node)| {
                let target = &self.nodes[node as usize];
                let score = self.metric.score(
                    &owner_node.vector,
                    &target.vector,
                    owner_node.norm_sq,
                    target.norm_sq,
                );
                (score, node)
            })
            .collect();
        refined.sort_by(|left, right| {
            self.metric
                .score_order(left.0, right.0)
                .then(left.1.cmp(&right.1))
        });
        refined
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
            for &chosen in selected.iter().take(self.compare_cap) {
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

    /// 修剪超员节点的邻集,并**强制保留**邻居 `protect`(修复路径用;必要时挤掉
    /// 选中序列的最后一位),度数恒在上界内。
    fn prune_protecting(&mut self, node: u32, level: usize, protect: u32) {
        let max_conn = if level == 0 { self.m0 } else { self.m };
        let mut selected = self.compute_pruned(node, level);
        if !selected.contains(&protect) {
            if selected.len() >= max_conn {
                selected.pop();
            }
            selected.push(protect);
        }
        // reason: 构建路径的图恒为 `Heap`(载入路径不经此函数)。
        if let Some(graph) = self.graph.heap_mut() {
            graph.set_neighbors(node, level, selected);
        }
    }
}

impl VectorIndex for HnswIndex {
    fn node_count(&self) -> usize {
        self.nodes.len()
    }

    fn max_level(&self) -> u8 {
        self.graph.max_level()
    }

    fn entry(&self) -> (SlotId, u8) {
        if self.nodes.is_empty() {
            return (SlotId::new(0), 0);
        }
        (
            self.slot_of[self.graph.entry() as usize],
            self.graph.entry_level(),
        )
    }

    fn serialize(&self) -> Result<Vec<u8>> {
        // 惰性图在序列化时物化为堆图(仅 flush/诊断路径;查询不走此路)。
        let graph = self.graph.to_graph();
        hidx::encode(
            &graph,
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
        let index =
            HnswIndex::build_with_options(&nodes, params, Metric::Dot, BuildPrecision::Hybrid, 4)
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
        let single =
            HnswIndex::build_with_options(&nodes, params, Metric::Dot, BuildPrecision::Hybrid, 1)
                .expect("单线程构建");
        let parallel =
            HnswIndex::build_with_options(&nodes, params, Metric::Dot, BuildPrecision::Hybrid, 4)
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
        let exact =
            HnswIndex::build_with_precision(&nodes, params, Metric::Dot, BuildPrecision::F32)
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
        let error = HnswIndex::load(
            &hidx_span(&bytes),
            &nodes[..2],
            &slot_of[..2],
            Metric::Dot,
            None,
        )
        .err()
        .expect("节点数不一致必须拒绝载入");
        assert!(matches!(
            error,
            crate::core::error::MnemeError::Corrupted { .. }
        ));
        // `slot_of` 长度不一致同样拒绝。
        let error = HnswIndex::load(&hidx_span(&bytes), &nodes, &slot_of[..2], Metric::Dot, None)
            .err()
            .expect("槽位数不一致必须拒绝载入");
        assert!(matches!(
            error,
            crate::core::error::MnemeError::Corrupted { .. }
        ));
        // 边界对照:完全一致时可载入。
        assert!(HnswIndex::load(&hidx_span(&bytes), &nodes, &slot_of, Metric::Dot, None).is_ok());
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
        assert!(HnswIndex::load(&hidx_span(&bytes), &[], &[], Metric::Dot, None).is_ok());
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
        let copy = |format: VectorFormat, rows: crate::memory::lazy::LazyRows, params: Vec<f32>| {
            QuantCopy {
                format,
                params,
                rows,
            }
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
}
