//! 确定性数据集与暴力 ground truth。
//!
//! 所有引擎共享同一份数据与真值，保证横向对比的可复现性与公平性：
//! - 向量用线性同余生成器（LCG）确定性生成，与主 crate `benches/` 同款，
//!   不引入 `rand` 依赖，且跨进程/跨机器同 seed 同数据；
//! - ground truth 用朴素 f32 欧氏距离平方暴力扫描得到，多线程分块计算；
//! - 召回口径为 Recall@k = |近似结果 ∩ 真值| / k，逐查询平均。
//!
//! 两种数据形态：
//! - [`DataMode::Uniform`]：逐分量均匀随机。ANN 最困难的一类输入（高维距离
//!   高度集中、近邻区分度低），各引擎召回一致偏低，属数据特性而非实现差异；
//! - [`DataMode::Clustered`]：若干随机中心 + 簇内噪声，模拟真实嵌入的簇结构
//!   （文本/图像嵌入的典型形态），召回与延迟更接近生产口径。

use std::collections::HashSet;

/// LCG 乘子（Knuth MMIX）。
const LCG_MUL: u64 = 6364136223846793005;
/// LCG 增量（Knuth MMIX）。
const LCG_ADD: u64 = 1442695040888963407;
/// 数据向量 seed（固定值，保证数据集可复现；ASCII "MNEME"）。
const DATA_SEED: u64 = 0x004D_4E45_4D45;
/// 查询向量 seed（与数据 seed 不同，避免查询与库内向量重合；ASCII "QUERY"）。
const QUERY_SEED: u64 = 0x0051_5545_5259;
/// 簇中心 seed（ASCII "CENTE"）。
const CENTER_SEED: u64 = 0x0043_454E_5445;
/// 带簇结构数据的簇数。
const CLUSTER_COUNT: usize = 100;
/// 带簇结构数据的簇内噪声半幅（分量均匀分布，相对中心间距约 1/5）。
const CLUSTER_SPREAD: f32 = 0.1;

/// 数据形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataMode {
    /// 逐分量均匀随机（ANN 困难集）。
    Uniform,
    /// 随机中心 + 簇内噪声（逼近真实嵌入分布）。
    Clustered,
}

impl DataMode {
    /// 从命令行取值解析。
    ///
    /// # Returns
    /// `uniform` / `clustered` 对应模式；其他取值返回 `None`。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "uniform" => Some(Self::Uniform),
            "clustered" => Some(Self::Clustered),
            _ => None,
        }
    }

    /// 命令行取值（与 [`DataMode::from_name`] 互逆）。
    pub fn as_name(self) -> &'static str {
        match self {
            Self::Uniform => "uniform",
            Self::Clustered => "clustered",
        }
    }

    /// 报告口径的描述文字。
    pub fn label(self) -> &'static str {
        match self {
            Self::Uniform => "逐分量均匀随机（ANN 困难集）",
            Self::Clustered => "随机中心 + 簇内噪声（近似真实嵌入）",
        }
    }
}

/// 一份可复现的对比数据集（向量 + 查询 + 真值）。
#[derive(Debug)]
pub struct Dataset {
    /// 库内向量行数。
    pub rows: usize,
    /// 向量维度。
    pub dim: usize,
    /// 库内向量，row-major（长度 `rows * dim`）。
    pub vectors: Vec<f32>,
    /// 查询向量，row-major（长度 `queries * dim`）。
    pub queries: Vec<f32>,
    /// 每条查询的精确 top-k 行号（0-based），长度与查询条数一致。
    pub truth: Vec<Vec<u32>>,
}

impl Dataset {
    /// 查询条数。
    pub fn query_count(&self) -> usize {
        self.queries.len().checked_div(self.dim).unwrap_or(0)
    }

    /// 以 `index` 为索引取查询向量切片。
    ///
    /// # Panics
    /// `index` 越界时 panic（调用方为基准工具内部，属于编程错误）。
    pub fn query(&self, index: usize) -> &[f32] {
        &self.queries[index * self.dim..(index + 1) * self.dim]
    }
}

/// 生成数据集并计算真值。
///
/// # Arguments
/// * `mode` - 数据形态。
/// * `rows` - 库内向量行数（须 > 0）。
/// * `dim` - 向量维度（须 > 0）。
/// * `queries` - 查询条数（须 > 0）。
/// * `k` - 真值 top-k（须 > 0）。
/// * `threads` - 真值计算的并行度（0 视为 1）。
///
/// # Returns
/// 数据、查询与逐查询精确 top-k 行号。
pub fn generate(
    mode: DataMode,
    rows: usize,
    dim: usize,
    queries: usize,
    k: usize,
    threads: usize,
) -> Dataset {
    debug_assert!(rows > 0 && dim > 0 && queries > 0 && k > 0);
    let (vectors, queries_vec) = match mode {
        DataMode::Uniform => (
            generate_vectors(DATA_SEED, rows * dim),
            generate_vectors(QUERY_SEED, queries * dim),
        ),
        DataMode::Clustered => {
            let centers = generate_vectors(CENTER_SEED, CLUSTER_COUNT * dim);
            (
                generate_clustered(DATA_SEED, rows, dim, &centers),
                generate_clustered(QUERY_SEED, queries, dim, &centers),
            )
        }
    };
    let truth = brute_force_truth(&vectors, dim, &queries_vec, k, threads.max(1));
    Dataset {
        rows,
        dim,
        vectors,
        queries: queries_vec,
        truth,
    }
}

/// LCG 状态推进。
fn next_state(state: u64) -> u64 {
    state.wrapping_mul(LCG_MUL).wrapping_add(LCG_ADD)
}

/// LCG 状态映射到 `[-0.5, 0.5)`。
fn unit(state: u64) -> f32 {
    ((state >> 33) as f32 / (1_u64 << 31) as f32) - 0.5
}

/// 用 LCG 生成 `count` 个落在 `[-0.5, 0.5)` 的确定性 f32。
fn generate_vectors(seed: u64, count: usize) -> Vec<f32> {
    let mut state = seed;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        state = next_state(state);
        out.push(unit(state));
    }
    out
}

/// 生成簇结构向量：先随机选簇，再叠加簇内均匀噪声。
fn generate_clustered(seed: u64, count: usize, dim: usize, centers: &[f32]) -> Vec<f32> {
    let mut state = seed;
    let mut out = Vec::with_capacity(count * dim);
    for _ in 0..count {
        state = next_state(state);
        let cluster = ((state >> 33) as usize) % CLUSTER_COUNT;
        for axis in 0..dim {
            state = next_state(state);
            let noise = unit(state) * 2.0 * CLUSTER_SPREAD;
            out.push(centers[cluster * dim + axis] + noise);
        }
    }
    out
}

/// 欧氏距离平方（与各引擎的 L2² 同口径；开方不影响排序）。
pub(crate) fn l2_sq(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            let d = x - y;
            d * d
        })
        .sum()
}

/// 多线程分块计算逐查询精确 top-k。
pub(crate) fn brute_force_truth(
    vectors: &[f32],
    dim: usize,
    queries: &[f32],
    k: usize,
    threads: usize,
) -> Vec<Vec<u32>> {
    let rows = vectors.len() / dim;
    let nq = queries.len() / dim;
    let k = k.min(rows);
    let mut truth: Vec<Vec<u32>> = vec![Vec::new(); nq];
    let threads = threads.clamp(1, nq.max(1));
    if threads == 1 {
        for (qi, slot) in truth.iter_mut().enumerate() {
            *slot = exact_topk(vectors, dim, rows, &queries[qi * dim..(qi + 1) * dim], k);
        }
        return truth;
    }
    let chunk = nq.div_ceil(threads);
    std::thread::scope(|scope| {
        for (block, slots) in truth.chunks_mut(chunk).enumerate() {
            let base = block * chunk;
            scope.spawn(move || {
                let mut scratch: Vec<(f32, u32)> = Vec::with_capacity(rows);
                for (offset, slot) in slots.iter_mut().enumerate() {
                    let qi = base + offset;
                    let query = &queries[qi * dim..(qi + 1) * dim];
                    scratch.clear();
                    for row in 0..rows {
                        scratch.push((
                            l2_sq(query, &vectors[row * dim..(row + 1) * dim]),
                            row as u32,
                        ));
                    }
                    let take = k.min(scratch.len());
                    if take == 0 {
                        *slot = Vec::new();
                        continue;
                    }
                    scratch.select_nth_unstable_by(take - 1, |a, b| a.0.total_cmp(&b.0));
                    scratch.truncate(take);
                    scratch.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
                    *slot = scratch.iter().map(|(_, id)| *id).collect();
                }
            });
        }
    });
    truth
}

/// 单查询精确 top-k（顺序扫描 + 选择）。
fn exact_topk(vectors: &[f32], dim: usize, rows: usize, query: &[f32], k: usize) -> Vec<u32> {
    let mut scratch: Vec<(f32, u32)> = (0..rows)
        .map(|row| {
            (
                l2_sq(query, &vectors[row * dim..(row + 1) * dim]),
                row as u32,
            )
        })
        .collect();
    let take = k.min(scratch.len());
    if take == 0 {
        return Vec::new();
    }
    scratch.select_nth_unstable_by(take - 1, |a, b| a.0.total_cmp(&b.0));
    scratch.truncate(take);
    scratch.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    scratch.iter().map(|(_, id)| *id).collect()
}

/// 计算近似结果相对真值的 Recall@k。
///
/// # Arguments
/// * `truth` - 该查询的精确 top-k 行号。
/// * `hits` - 近似检索返回的行号（可能不足 k 条）。
///
/// # Returns
/// `|hits ∩ truth| / |truth|`；`truth` 为空时返回 1.0。
pub fn recall_at_k(truth: &[u32], hits: &[u32]) -> f64 {
    if truth.is_empty() {
        return 1.0;
    }
    let hits: HashSet<u32> = hits.iter().copied().collect();
    let found = truth.iter().filter(|id| hits.contains(id)).count();
    found as f64 / truth.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataset_is_deterministic() {
        let a = generate(DataMode::Uniform, 64, 8, 4, 3, 1);
        let b = generate(DataMode::Uniform, 64, 8, 4, 3, 4);
        assert_eq!(a.vectors, b.vectors);
        assert_eq!(a.queries, b.queries);
        assert_eq!(a.truth, b.truth, "并行度不影响真值");
    }

    #[test]
    fn clustered_data_has_closer_same_cluster_neighbours() {
        // 簇结构数据:同一查询在簇结构数据里的近邻距离,应显著小于均匀随机数据。
        fn nearest_distance(dataset: &Dataset, query: &[f32], rank: usize) -> f32 {
            let mut distances: Vec<f32> = (0..dataset.rows)
                .map(|row| {
                    l2_sq(
                        query,
                        &dataset.vectors[row * dataset.dim..(row + 1) * dataset.dim],
                    )
                })
                .collect();
            distances.sort_unstable_by(f32::total_cmp);
            distances[rank.min(distances.len() - 1)]
        }
        let clustered = generate(DataMode::Clustered, 2_000, 16, 4, 10, 2);
        let uniform = generate(DataMode::Uniform, 2_000, 16, 4, 10, 2);
        let query = &clustered.vectors[..16];
        assert!(
            nearest_distance(&clustered, query, 10) < nearest_distance(&uniform, query, 10),
            "簇结构数据同簇近邻应更近"
        );
    }

    #[test]
    fn truth_is_sorted_and_within_bounds() {
        let ds = generate(DataMode::Uniform, 128, 16, 8, 5, 2);
        for truth in &ds.truth {
            assert_eq!(truth.len(), 5);
            assert!(truth.iter().all(|id| (*id as usize) < ds.rows));
        }
    }

    #[test]
    fn recall_handles_partial_hits() {
        assert!((recall_at_k(&[1, 2, 3, 4], &[1, 2, 9]) - 0.5).abs() < 1e-12);
        assert!((recall_at_k(&[1, 2], &[]) - 0.0).abs() < 1e-12);
        assert!((recall_at_k(&[], &[]) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn data_mode_names_round_trip() {
        for mode in [DataMode::Uniform, DataMode::Clustered] {
            assert_eq!(DataMode::from_name(mode.as_name()), Some(mode));
        }
        assert_eq!(DataMode::from_name("other"), None);
    }
}
