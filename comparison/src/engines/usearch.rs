//! usearch 适配：C++ 核心 + 官方 Rust 绑定，L2² 度量，全 f32 存储。
//!
//! 构建口径 = `Index::new` + `reserve` + 并行 `add`（usearch 自身线程安全，
//! 与 Mneme 批内并行、hnsw_rs `parallel_insert` 对等）。

use ::usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use super::{BuildOptions, Engine, for_each_chunk_parallel};

/// usearch 引擎句柄。
pub struct UsearchEngine {
    index: Index,
}

impl Engine for UsearchEngine {
    fn build(
        rows: usize,
        dim: usize,
        vectors: &[f32],
        threads: usize,
        _efs: &[usize],
        _options: BuildOptions,
    ) -> Result<Self, String> {
        let options = IndexOptions {
            dimensions: dim,
            metric: MetricKind::L2sq,
            quantization: ScalarKind::F32,
            connectivity: 16,
            expansion_add: 200,
            expansion_search: 128,
            ..IndexOptions::default()
        };
        let index = Index::new(&options).map_err(|error| format!("创建索引失败: {error}"))?;
        index
            .reserve(rows)
            .map_err(|error| format!("预留容量失败: {error}"))?;
        for_each_chunk_parallel(rows, threads, |start, end| {
            for row in start..end {
                let vector = &vectors[row * dim..(row + 1) * dim];
                index
                    .add(row as u64, vector)
                    .map_err(|error| format!("add 第 {row} 行失败: {error}"))?;
            }
            Ok(())
        })?;
        Ok(Self { index })
    }

    fn name(&self) -> &'static str {
        "usearch"
    }

    fn version(&self) -> &'static str {
        // 与 Cargo.toml 的 usearch 依赖版本同步。
        "2.26.x"
    }

    fn search(&self, query: &[f32], k: usize, ef: usize) -> Result<Vec<u32>, String> {
        self.index.change_expansion_search(ef);
        let matches = self
            .index
            .search(query, k)
            .map_err(|error| format!("查询失败: {error}"))?;
        Ok(matches.keys.iter().map(|key| *key as u32).collect())
    }

    fn index_bytes(&self) -> Option<u64> {
        Some(self.index.memory_usage() as u64)
    }
}
