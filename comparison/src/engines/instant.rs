//! instant-distance 适配：InstantDomainSearch 生产使用的纯 Rust HNSW。
//!
//! 限制：`ef_search` 在构建期锁定（`Hnsw` 无运行时 setter），因此只参与
//! `--ef` 列表的**第一档**，其余档位在结果中标记为跳过；其 `M` 为库内常量 32。
//!
//! 查询状态：`Point` 要求 `Clone + Sync`，`Search` 每次查询新建（默认构造只有
//! 几个空 `Vec`，开销相对微秒级查询可忽略），从而避免内部可变状态拖累并发测量。

use ::instant_distance::{Builder, HnswMap, Point, Search};

use super::{BuildOptions, Engine};
use crate::dataset::l2_sq;

/// 以 `Vec<f32>` 承载的点类型（`Point` 要求 `Clone + Sync`）。
#[derive(Clone, Debug)]
pub struct InstantPoint(Vec<f32>);

impl Point for InstantPoint {
    fn distance(&self, other: &Self) -> f32 {
        l2_sq(&self.0, &other.0)
    }
}

/// instant-distance 引擎句柄。
pub struct InstantEngine {
    map: HnswMap<InstantPoint, u32>,
    ef: usize,
}

impl Engine for InstantEngine {
    fn build(
        rows: usize,
        dim: usize,
        vectors: &[f32],
        _threads: usize,
        efs: &[usize],
        _options: BuildOptions,
    ) -> Result<Self, String> {
        let ef = *efs
            .first()
            .ok_or_else(|| "instant-distance 需要至少一档 ef".to_string())?;
        let points: Vec<InstantPoint> = (0..rows)
            .map(|row| InstantPoint(vectors[row * dim..(row + 1) * dim].to_vec()))
            .collect();
        let values: Vec<u32> = (0..rows as u32).collect();
        let map = Builder::default()
            .ef_construction(200)
            .ef_search(ef)
            .seed(42)
            .build(points, values);
        Ok(Self { map, ef })
    }

    fn name(&self) -> &'static str {
        "instant_distance"
    }

    fn version(&self) -> &'static str {
        // 与 Cargo.toml 的 instant-distance 依赖版本同步。
        "0.6.x"
    }

    fn search(&self, query: &[f32], k: usize, ef: usize) -> Result<Vec<u32>, String> {
        if ef != self.ef {
            return Err(format!(
                "instant-distance 构建期锁定 ef={},不支持运行时切换 ef={ef}",
                self.ef
            ));
        }
        let point = InstantPoint(query.to_vec());
        let mut search = Search::default();
        let hits = self
            .map
            .search(&point, &mut search)
            .take(k)
            .map(|item| *item.value)
            .collect();
        Ok(hits)
    }

    fn supports_ef(&self, ef: usize) -> bool {
        ef == self.ef
    }
}
