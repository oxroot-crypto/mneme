//! L6 量化基准(criterion,设计 14 §4「量化收益」)。
//!
//! 对比 f32 与 i8 两阶段查询延迟(微缩规模 4k×512)与建库开销(8k×128);
//! 该规模下每查询的候选收集/位图等固定开销占比高,i8 的读侧带宽优势要到
//! 1M×1536 才充分体现——正式 ≥3× 加速门槛属 CI heavy 档(仓库当前尚无 CI 配置)。
//! 召回损失验收见 `tests/l6_contracts.rs`。

use std::hint::black_box;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use mneme::{Builder, Metric, Record, Tuning, VectorFormat};

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

/// 建一个已 flush(已建 HNSW 与量化副本)的临时持久库。
fn build_db(rows: usize, dim: usize, format: VectorFormat) -> (mneme::Mneme, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(dim as u32)
        .metric(Metric::Dot)
        .quantization(format)
        .tuning(Tuning {
            brute_force_max_rows: 64,
            quant_recall_floor: 0.0,
            ..Tuning::default()
        })
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

/// 查询延迟:f32 精确路径 vs i8 两阶段(量化粗排 + f32 精排)。
///
/// 512 维、4k 行:点积在 L3 驻留下带宽占主导,能体现 i8 副本的读侧优势;
/// 1M×1536 的正式门槛属 CI heavy 档。
fn bench_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("quant_query_top10");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    let query = vector(999_999, 512);
    for (label, format) in [("f32", VectorFormat::F32), ("i8", VectorFormat::I8Rescored)] {
        let (db, _dir) = build_db(4_000, 512, format);
        let ns = db.namespace("bench");
        group.bench_with_input(BenchmarkId::from_parameter(label), &(), |b, ()| {
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
    group.finish();
}

/// 建库吞吐:插入 8k×128 并 flush(f32 vs 量化副本生成开销)。
fn bench_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("quant_build");
    group.sample_size(10);
    for format in [VectorFormat::F32, VectorFormat::I8Rescored] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{format:?}")),
            &format,
            |b, &format| {
                b.iter(|| {
                    let (_db, _dir) = build_db(8_000, 128, format);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_query, bench_build);
criterion_main!(benches);
