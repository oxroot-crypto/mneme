//! 分批并行建图:批计划、确定性层级骰子与构建入口。

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::options::HnswBuildParams;
use crate::core::types::SlotId;
use crate::index::graph::{Graph, GraphStore};
use crate::memory::index::{IndexNode, MAX_INDEX_DEGREE, QuantQuery};

use super::HnswIndex;
use super::QueryRef;
use super::model::LinkPlan;
use super::quant::{build_codes, cached_i8_params, validate_quant};

#[cfg(test)]
use crate::core::metric::Metric;
#[cfg(test)]
use crate::core::options::BuildPrecision;
#[cfg(test)]
use crate::core::options::HnswParams;

mod inputs;
mod plan;
mod pool;

// 拆分唔改外部可达性:旧路径 `crate::index::hnsw::build::{...}` 照旧经重导出可达。
#[cfg(test)]
pub(in crate::index::hnsw) use inputs::BuildWithOptionsInput;
pub(crate) use inputs::BuildWithSlotsInput;
pub(in crate::index::hnsw) use plan::{build_batch_rows, build_first_batch_rows, build_threads};

/// 构建期确定性种子(固定 -> 同输入同图,便于测试与回归)。
const BUILD_SEED: u64 = 0x4D4E_454D_4500_0001;
/// 层级骰子上限(防 `-ln(u)` 极端值导致层级爆炸)。
const MAX_ROLL_LEVEL: usize = 31;

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
        Self::build_with_precision(nodes, params, metric, BuildPrecision::default())
            .expect("测试输入恒满足建图前置")
    }

    /// 测试辅助:指定精度档位构建(恒等槽位映射;并行度按可用核数)。
    #[cfg(test)]
    pub(super) fn build_with_precision(
        nodes: &[IndexNode],
        params: HnswParams,
        metric: Metric,
        precision: BuildPrecision,
    ) -> Result<Self> {
        Self::build_with_options(BuildWithOptionsInput {
            nodes,
            params,
            metric,
            precision,
            parallelism: 0,
        })
    }

    /// 测试辅助:显式指定精度档位与并行度构建(恒等槽位映射;其余建图参数用默认)。
    #[cfg(test)]
    pub(super) fn build_with_options(input: BuildWithOptionsInput<'_>) -> Result<Self> {
        let BuildWithOptionsInput {
            nodes,
            params,
            metric,
            precision,
            parallelism,
        } = input;
        let slot_of: Vec<SlotId> = (0..nodes.len()).map(|id| SlotId::new(id as u32)).collect();
        Self::build_with_slots(BuildWithSlotsInput {
            nodes,
            slot_of: &slot_of,
            params,
            metric,
            quant: None,
            precision,
            build: HnswBuildParams {
                parallelism,
                ..HnswBuildParams::default()
            },
        })
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
    pub(crate) fn build_with_slots(input: BuildWithSlotsInput<'_>) -> Result<Self> {
        let BuildWithSlotsInput {
            nodes,
            slot_of,
            params,
            metric,
            quant,
            precision,
            build,
        } = input;
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
        // 阶段 0–2:预分配节点与层级 -> 批内并行计算计划、批间串行应用。
        let levels = index.push_nodes_and_levels(nodes, slot_of, ml);
        index.run_build_batches(&levels, &build)?;
        // 阶段 3:可达性修复(批内互不可见可能留下极少量孤立)。
        index.repair_unreachable();
        // 临时码流只在构建期生存(不落盘、不进查询路径)。
        index.build = None;
        Ok(index)
    }

    /// 阶段 0:按确定性 RNG 顺序预分配节点、槽位与层级(此刻图无边)。
    ///
    /// `ml` 为层级骰子乘数(`1/ln(m)`);返回与 `nodes` 等长的层级表。
    fn push_nodes_and_levels(
        &mut self,
        nodes: &[IndexNode],
        slot_of: &[SlotId],
        ml: f32,
    ) -> Vec<u8> {
        let count = nodes.len().min(slot_of.len());
        let mut rng = Rng::new(BUILD_SEED);
        let mut levels = Vec::with_capacity(count);
        for position in 0..count {
            let level = roll_level(&mut rng, ml);
            levels.push(level);
            self.nodes.push(nodes[position].clone());
            self.slot_of.push(slot_of[position]);
            // reason: 构建路径的图恒为 `Heap`(载入路径不经此函数);`expect` 可证明不可达。
            self.graph
                .heap_mut()
                .expect("构建路径恒为堆图")
                .push_node(level);
        }
        levels
    }

    /// 阶段 1/2:批内并行计算计划、批间按节点序串行应用(结果与线程数无关)。
    ///
    /// 首批受 `m0` 约束:冷启动核心的入边不被集中修剪,保证从入口可达。
    /// 单线程退化为纯串行路径;多线程经常驻 worker 池执行(见 [`pool`] 模块),
    /// 批次间复用 worker,避免逐批创建/销毁线程。
    ///
    /// # Errors
    /// 计划计算失败(档位量化不一致)或构建 worker 退出时返回结构化错误。
    fn run_build_batches(&mut self, levels: &[u8], build: &HnswBuildParams) -> Result<()> {
        let count = levels.len();
        let batch_rows = build_batch_rows(count, build);
        let first_batch = build_first_batch_rows(batch_rows, self.m0);
        let threads = build_threads(build.parallelism, batch_rows, build.threads_max);
        if threads <= 1 {
            return self.run_build_batches_serial(levels, batch_rows, first_batch);
        }
        self.run_build_batches_pooled(levels, batch_rows, first_batch, threads)
    }

    /// 串行路径:逐批内联计算计划并应用(单线程/小图)。
    fn run_build_batches_serial(
        &mut self,
        levels: &[u8],
        batch_rows: usize,
        first_batch: usize,
    ) -> Result<()> {
        let count = levels.len();
        let mut start = 0;
        while start < count {
            let size = if start == 0 { first_batch } else { batch_rows };
            let end = (start + size).min(count);
            let plans: Vec<LinkPlan> = (start..end)
                .map(|position| self.plan_node(position as u32, levels[position]))
                .collect::<Result<_>>()?;
            self.apply_batch(start, &plans, levels);
            start = end;
        }
        Ok(())
    }

    /// 并行路径:常驻 worker 池计算计划/修剪,主线程在阶段之间持写锁应用。
    ///
    /// 锁纪律(避免死锁):计划与修剪阶段 worker 持读锁、主线程等待结果;
    /// 主线程只在两阶段之间持写锁,持锁期间不向 worker 派活。
    fn run_build_batches_pooled(
        &mut self,
        levels: &[u8],
        batch_rows: usize,
        first_batch: usize,
        threads: usize,
    ) -> Result<()> {
        let count = levels.len();
        let lock = std::sync::RwLock::new(self);
        std::thread::scope(|scope| -> Result<()> {
            let pool = pool::BuildPool::spawn(scope, threads, &lock, levels);
            let mut start = 0;
            while start < count {
                let size = if start == 0 { first_batch } else { batch_rows };
                let end = (start + size).min(count);
                let plans = pool.plan_batch(start, end)?;
                let touched = {
                    let mut guard = lock
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.apply_batch_links(start, &plans, levels)
                };
                let pruned = pool.prune_batch(&touched)?;
                {
                    let mut guard = lock
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.write_back_pruned(touched, pruned);
                }
                start = end;
            }
            pool.shutdown();
            Ok(())
        })
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
}
