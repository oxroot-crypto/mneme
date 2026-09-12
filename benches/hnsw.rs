//! L3 HNSW 基准(criterion,设计 14 §4 / 05 §6)。
//!
//! 产出构建吞吐与查询延迟的趋势样本(当前为 1k/8k×64 维微缩规模,设计 14 §4 的
//! 1M×1536 门槛待 CI heavy 档);召回门槛见 `tests/hnsw_contracts.rs`。
//! 阈值与回归门禁由 CI 判定(相对基线 > 10% 阻断),仓库当前尚无 CI 配置。

use std::hint::black_box;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use mneme::{Builder, Metric, Record};

/// 生成确定性均匀向量(线性同余,避免依赖 `rand`)。
fn vector(seed: u64, dim: usize) -> Vec<f32> {
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (0..dim)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f32 / (1_u64 << 31) as f32) - 0.5
        })
        .collect()
}

/// 建一个已 flush(已建 HNSW)的临时持久库,返回句柄与临时目录。
fn build_db(rows: usize, dim: usize) -> (mneme::Mneme, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(dim as u32)
        .metric(Metric::Dot)
        .build()
        .expect("build");
    let ns = db.namespace("bench");
    let batch: Vec<Record> = (0..rows)
        .map(|row| Record::new(vector(row as u64, dim)))
        .collect();
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");
    (db, dir)
}

/// 构建吞吐:插入 `rows` 条并 flush(建立 HNSW)。
fn bench_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("hnsw_build");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    for rows in [1_000usize, 8_000] {
        group.bench_with_input(BenchmarkId::from_parameter(rows), &rows, |b, &rows| {
            b.iter(|| {
                let (_db, _dir) = build_db(rows, 64);
            });
        });
    }
    group.finish();
}

/// 查询延迟:在已建 HNSW 的库上执行 top-10 向量检索。
fn bench_query(c: &mut Criterion) {
    let (db, _dir) = build_db(8_000, 64);
    let ns = db.namespace("bench");
    let query = vector(999_999, 64);
    c.bench_function("hnsw_query_top10", |b| {
        b.iter(|| {
            let hits = ns
                .search()
                .vector(black_box(&query))
                .top_k(10)
                .ef(64)
                .execute()
                .expect("search");
            black_box(hits);
        });
    });
}

criterion_group!(benches, bench_build, bench_query);
criterion_main!(benches);
