//! hnsw_rs 适配：纯 Rust HNSW，L2 距离，`parallel_insert` 并行建图。
//!
//! 注意：hnsw_rs 的 `parallel_insert` 使用 rayon 全局线程池，线程数不受
//! `--threads` 直接控制（默认 = 核数），报告备注。

use ::hnsw_rs::prelude::{DistL2, Hnsw, Neighbour};

use super::{BuildOptions, Engine};

/// hnsw_rs 引擎句柄。
pub struct HnswRsEngine {
    hnsw: Hnsw<'static, f32, DistL2>,
}

impl Engine for HnswRsEngine {
    fn build(
        _rows: usize,
        dim: usize,
        vectors: &[f32],
        _threads: usize,
        _efs: &[usize],
        _options: BuildOptions,
    ) -> Result<Self, String> {
        let rows = vectors.len() / dim;
        let mut hnsw = Hnsw::new(16, rows, 16, 200, DistL2);
        // hnsw_rs 的插入接口按「每条向量一个切片」组织,先把 row-major 拆成引用切片。
        let slices: Vec<(&[f32], usize)> = (0..rows)
            .map(|row| (&vectors[row * dim..(row + 1) * dim], row))
            .collect();
        hnsw.parallel_insert_slice(&slices);
        // 并行插入后必须切到 searching 模式,才允许查询(hnsw_rs 文档)。
        hnsw.set_searching_mode(true);
        Ok(Self { hnsw })
    }

    fn name(&self) -> &'static str {
        "hnsw_rs"
    }

    fn version(&self) -> &'static str {
        // 与 Cargo.toml 的 hnsw_rs 依赖版本同步。
        "0.3.x"
    }

    fn search(&self, query: &[f32], k: usize, ef: usize) -> Result<Vec<u32>, String> {
        let neighbours: Vec<Neighbour> = self.hnsw.search(query, k, ef);
        Ok(neighbours.iter().map(|n| n.d_id as u32).collect())
    }
}
