//! 建库吞吐与查询延迟门槛(heavy 档,设计 14 §4;`FC-GLOBAL-CPLX-001`)。
//!
//! 门槛:建库吞吐 ≥ 50k 向量/秒;查询(i8 量化,ef=128,top-10)P50 < 2ms、
//! P99 < 10ms。规模由 `MNEME_HEAVY_ROWS`/`MNEME_HEAVY_DIM` 配置:CI heavy 档
//! 显式以 1M×1536 运行并断言门槛;默认 50_000×128 只做冒烟(打印测量值,
//! 不判门槛——小规模固定开销占比高,不构成性能证据)。
//! 召回门槛由 `tests/hnsw_contracts.rs` 以微缩双分布验收;冷启动门槛见
//! `tests/cold_start.rs`。本文件锚定 `FC-GLOBAL-CPLX-001`,无独立 I 编号。
//!
//! 默认 `#[ignore]`;heavy CI 用
//! `MNEME_HEAVY=1 cargo test --release --test heavy_gate -- --ignored` 执行;
//! `MNEME_HEAVY` 未设置时显式失败,绝不静默跳过(配错 env 的 CI 不得假绿)。

mod common;

use std::time::{Duration, Instant};

use mneme::{Builder, FsyncPolicy, Metric, Record, VectorFormat};

use common::{heavy_dimension, heavy_rows, heavy_vector};

/// 单批写入条数(控制建库期内存峰值)。
const BATCH: usize = 10_000;
/// 正式门槛规模(CI heavy 档);仅达到该规模才断言吞吐/延迟门槛。
const OFFICIAL_ROWS: usize = 1_000_000;
/// 正式门槛维度。
const OFFICIAL_DIMENSION: usize = 1536;
/// 延迟采样次数(预热后)。
const SAMPLES: usize = 200;
/// 建库吞吐门槛(向量/秒)。
const BUILD_THROUGHPUT_PER_SEC: f64 = 50_000.0;
/// 查询 P50 门槛。
const QUERY_P50: Duration = Duration::from_millis(2);
/// 查询 P99 门槛。
const QUERY_P99: Duration = Duration::from_millis(10);

/// 建指定规模的库并 flush,返回句柄与临时目录。
///
/// 建库测量期不启动后台维护(`maintenance(false)`):自动 compaction 合并段会
/// 与导入/flush 争抢 CPU/IO,污染"建库吞吐"测量;门槛只约束导入与建索引路径。
fn build_heavy(rows: usize, dimension: usize) -> (mneme::Mneme, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .dimension(dimension as u32)
        .metric(Metric::Dot)
        .fsync(FsyncPolicy::OnFlush)
        .quantization(VectorFormat::I8Rescored)
        .maintenance(false)
        .path(dir.path())
        .build()
        .expect("build");
    let ns = db.namespace("heavy");
    let mut written = 0;
    while written < rows {
        let take = BATCH.min(rows - written);
        let batch: Vec<Record> = (0..take)
            .map(|offset| {
                let row = (written + offset) as u64;
                Record::new(heavy_vector(row, dimension)).key(format!("k{row}"))
            })
            .collect();
        ns.insert_batch(batch).expect("insert_batch");
        written += take;
    }
    (db, dir)
}

/// 从延迟样本计算分位值(样本已排序)。
fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

/// **设计 14 §4**:正式规模(1M×1536)下建库吞吐 ≥ 50k/s、i8 ef=128 top-10
/// P50 < 2ms、P99 < 10ms;默认小规模打印测量值、只验正确性。
#[test]
#[ignore = "heavy:需显式 MNEME_HEAVY=1;规模由 MNEME_HEAVY_ROWS/MNEME_HEAVY_DIM 配置"]
fn heavy_perf_gates() {
    if std::env::var("MNEME_HEAVY").as_deref() != Ok("1") {
        panic!("MNEME_HEAVY=1 未设置:heavy 门槛不应静默跳过(显式失败,杜绝 CI 假绿)");
    }
    let rows = heavy_rows();
    let dimension = heavy_dimension();
    let enforce_gates = rows >= OFFICIAL_ROWS && dimension >= OFFICIAL_DIMENSION;

    let started = Instant::now();
    let (db, _dir) = build_heavy(rows, dimension);
    let build = started.elapsed();
    let throughput = rows as f64 / build.as_secs_f64();
    if enforce_gates {
        assert!(
            throughput >= BUILD_THROUGHPUT_PER_SEC,
            "建库吞吐 {throughput:.0}/s 低于门槛 {BUILD_THROUGHPUT_PER_SEC:.0}/s(耗时 {build:?})"
        );
    } else {
        eprintln!(
            "冒烟规模 {rows}×{dimension}:建库吞吐 {throughput:.0}/s(门槛仅在正式规模 {OFFICIAL_ROWS}×{OFFICIAL_DIMENSION} 断言)"
        );
    }

    let ns = db.namespace("heavy");
    let queries: Vec<Vec<f32>> = (0..SAMPLES)
        .map(|index| heavy_vector(rows as u64 + index as u64 + 1, dimension))
        .collect();
    let search = |query: &[f32]| {
        ns.search()
            .vector(query)
            .top_k(10)
            .ef(128)
            .execute()
            .expect("search")
    };
    let _ = search(&queries[0]);
    let mut latencies: Vec<Duration> = queries
        .iter()
        .map(|query| {
            let started = Instant::now();
            let hits = search(query);
            let elapsed = started.elapsed();
            assert_eq!(hits.len(), 10);
            elapsed
        })
        .collect();
    latencies.sort_unstable();
    let p50 = percentile(&latencies, 0.50);
    let p99 = percentile(&latencies, 0.99);
    if enforce_gates {
        assert!(p50 < QUERY_P50, "查询 P50 {p50:?} 超过门槛 {QUERY_P50:?}");
        assert!(p99 < QUERY_P99, "查询 P99 {p99:?} 超过门槛 {QUERY_P99:?}");
    } else {
        eprintln!(
            "冒烟规模 {rows}×{dimension}:查询 P50 {p50:?}、P99 {p99:?}(门槛仅在正式规模 {OFFICIAL_ROWS}×{OFFICIAL_DIMENSION} 断言)"
        );
    }
    db.close().expect("close");
}
