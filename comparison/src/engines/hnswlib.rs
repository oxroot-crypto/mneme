//! hnsw-stable 适配：经典 C++ hnswlib 的纯 Rust 移植（stable 工具链分支），
//! L2 度量；`Hnsw` 支持并发插入（`&self`），与其余引擎的构建并行度对等。

use ::hnsw_stable::{Hnsw, HnswConfig, InMemoryVectorStore, L2};

use super::{BuildOptions, Engine, for_each_chunk_parallel};

/// hnsw-stable 引擎句柄（图 + 向量存储分开持有）。
pub struct HnswStableEngine {
    hnsw: Hnsw<u32, L2>,
    vectors: InMemoryVectorStore<f32>,
}

impl Engine for HnswStableEngine {
    fn build(
        rows: usize,
        dim: usize,
        vectors: &[f32],
        threads: usize,
        _efs: &[usize],
        _options: BuildOptions,
    ) -> Result<Self, String> {
        let config = HnswConfig::new(dim, rows)
            .m(16)
            .ef_construction(200)
            .ef_search(128);
        let hnsw = Hnsw::<u32, L2>::new(L2::new(), config);
        let store = InMemoryVectorStore::<f32>::new(dim, rows);
        for_each_chunk_parallel(rows, threads, |start, end| {
            for row in start..end {
                let vector = &vectors[row * dim..(row + 1) * dim];
                hnsw.insert(&store, row as u32, vector)
                    .map_err(|error| format!("insert 第 {row} 行失败: {error}"))?;
            }
            Ok(())
        })?;
        Ok(Self {
            hnsw,
            vectors: store,
        })
    }

    fn name(&self) -> &'static str {
        "hnsw_stable"
    }

    fn version(&self) -> &'static str {
        // 与 Cargo.toml 的 hnsw-stable 依赖版本同步。
        "0.10.x"
    }

    fn search(&self, query: &[f32], k: usize, ef: usize) -> Result<Vec<u32>, String> {
        self.hnsw.set_ef_search(ef);
        let hits = self
            .hnsw
            .search(&self.vectors, query, k, None)
            .map_err(|error| format!("查询失败: {error}"))?;
        Ok(hits.iter().map(|hit| hit.key).collect())
    }
}
