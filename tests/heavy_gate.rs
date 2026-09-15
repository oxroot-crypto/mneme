//! 建库吞吐与查询延迟门槛(heavy 档,设计 14 §4;`FC-GLOBAL-CPLX-001`)。
//!
//! 门槛:建库吞吐 ≥ 300 向量/秒、查询(i8 量化,ef=128,top-10)P50 < 50ms、
//! P99 < 100ms(均锚定 4 核云 VM 实测,见常量说明)。规模由
//! `MNEME_HEAVY_ROWS`/`MNEME_HEAVY_DIM` 配置:CI heavy 档显式以 1M×1536 运行
//! 并断言门槛;默认 50_000×128 只做冒烟(打印吞吐/延迟/Recall@10 全方位测量值,
//! 不判门槛——小规模固定开销占比高,不构成性能证据)。冒烟先显式 `flush()` 让
//! 索引覆盖全部槽位,保证测的是 ANN 路径而非未落盘尾部暴力。
//! 召回门槛由 `tests/hnsw_contracts.rs` 以微缩双分布验收;冷启动门槛见
//! `tests/cold_start.rs`。本文件锚定 `FC-GLOBAL-CPLX-001`,无独立 I 编号。
//!
//! 默认 `#[ignore]`;heavy CI 用
//! `MNEME_HEAVY=1 cargo test --release --test heavy_gate -- --ignored` 执行;
//! `MNEME_HEAVY` 未设置时显式失败,绝不静默跳过(配错 env 的 CI 不得假绿)。

mod common;

use std::time::{Duration, Instant};

use mneme::{Builder, FsyncPolicy, Metric, Record, Tuning, VectorFormat};

use common::{heavy_dimension, heavy_rows, heavy_vector, tuning_with_env};

/// 单批写入条数(控制建库期内存峰值)。
const BATCH: usize = 10_000;
/// 正式门槛规模(CI heavy 档);仅达到该规模才断言吞吐/延迟门槛。
const OFFICIAL_ROWS: usize = 1_000_000;
/// 正式门槛维度。
const OFFICIAL_DIMENSION: usize = 1536;
/// 延迟采样次数(预热后)。
const SAMPLES: usize = 200;
/// 建库吞吐门槛(向量/秒):锚定 4 核云 VM 实测下限(1M×1536、efC=200/m=16、
/// Hybrid 建图:4 核 Xeon 8255C 实测 424/s,另一台机器 1187/s)。HNSW 建图
/// 距离计算受内存带宽约束,核数不线性放大吞吐,故取固定下限而非按核数缩放;
/// GPU/CAGRA 档 ≈50k+/s 属待可选 GPU 后端的远期目标,不参与本门槛。
const BUILD_THROUGHPUT_PER_SEC: f64 = 300.0;
/// 查询 P50 门槛:锚定 4 核云 VM 实测(1M×1536/16 段、ef=128、i8 粗排 +
/// 段间并行 + 视图级缓存:4 核 Xeon 8255C 实测 33ms),取约 1.5× 余量。
/// HNSW 逐段随机访问受内存延迟约束,段数 × 单段成本构成墙钟下限。
const QUERY_P50: Duration = Duration::from_millis(50);
/// 查询 P99 门槛:同口径实测分布 P99 较 P50 高约 1.5–2×,取 3× 余量。
const QUERY_P99: Duration = Duration::from_millis(100);

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
        .tuning(tuning_with_env(Tuning::default()))
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

/// **设计 14 §4**:正式规模(1M×1536)下建库吞吐 ≥ 300/s(4 核基准)、i8 ef=128
/// top-10 P50 < 50ms、P99 < 100ms;默认小规模打印测量值、只验正确性。
#[test]
#[ignore = "heavy:需显式 MNEME_HEAVY=1;规模由 MNEME_HEAVY_ROWS/MNEME_HEAVY_DIM 配置"]
fn heavy_perf_gates() {
    common::env::require_heavy();
    let rows = heavy_rows();
    let dimension = heavy_dimension();
    let enforce_gates = rows >= OFFICIAL_ROWS && dimension >= OFFICIAL_DIMENSION;

    let started = Instant::now();
    let (db, _dir) = build_heavy(rows, dimension);
    let build = started.elapsed();
    let throughput = rows as f64 / build.as_secs_f64();
    if enforce_gates {
        // 失败信息带可用核数:门槛为 4 核基准下限,便于区分「真回归」与「弱机/降频」。
        let cores = std::thread::available_parallelism().map_or(0, |count| count.get());
        assert!(
            throughput >= BUILD_THROUGHPUT_PER_SEC,
            "建库吞吐 {throughput:.0}/s 低于门槛 {BUILD_THROUGHPUT_PER_SEC:.0}/s(耗时 {build:?},可用核数 {cores})"
        );
    } else {
        eprintln!(
            "冒烟规模 {rows}×{dimension}:建库吞吐 {throughput:.0}/s(门槛仅在正式规模 {OFFICIAL_ROWS}×{OFFICIAL_DIMENSION} 断言)"
        );
    }

    // 显式 flush 让全部槽位被段索引覆盖:冒烟规模(低维/行数不足以触发 WAL
    // 阈值)此前查询走未落盘尾部暴力,延迟与召回不反映 ANN 路径。正式规模插入期
    // 已自动 flush 覆盖全部槽位,此处为空操作,不影响门槛口径。
    db.flush().expect("flush");

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
    let mut latencies: Vec<Duration> = Vec::with_capacity(SAMPLES);
    let mut ann_rows: Vec<Vec<u64>> = Vec::with_capacity(SAMPLES);
    for query in &queries {
        let started = Instant::now();
        let hits = search(query);
        let elapsed = started.elapsed();
        assert_eq!(hits.len(), 10);
        latencies.push(elapsed);
        ann_rows.push(hits.iter().map(|hit| hit.rowid.get()).collect());
    }
    latencies.sort_unstable();
    let p50 = percentile(&latencies, 0.50);
    let p99 = percentile(&latencies, 0.99);
    if enforce_gates {
        // 正式规模先回显全部测量值再断言:P50 不达标时不遮蔽 P99 数据。
        // 召回不在正式规模测:精确参考需全量 $O(N \cdot d)$ 打点(1M×1536 约
        // 数百秒),门槛召回由 `tests/hnsw_contracts.rs` 微缩双分布验收。
        eprintln!(
            "正式规模 {rows}×{dimension}:建库吞吐 {throughput:.0}/s、查询 P50 {p50:?}、P99 {p99:?}"
        );
        assert!(p50 < QUERY_P50, "查询 P50 {p50:?} 超过门槛 {QUERY_P50:?}");
        assert!(p99 < QUERY_P99, "查询 P99 {p99:?} 超过门槛 {QUERY_P99:?}");
    } else {
        eprintln!(
            "冒烟规模 {rows}×{dimension}:查询 P50 {p50:?}、P99 {p99:?}(门槛仅在正式规模 {OFFICIAL_ROWS}×{OFFICIAL_DIMENSION} 断言)"
        );
        // 召回-延迟曲线:同口径精确参考(见 `brute_top_k`);参考只算一次,
        // 各 ef 复用。冒烟成本 O(rows×dim×SAMPLES) 参考 + O(ef) 检索。
        let vectors: Vec<Vec<f32>> = (0..rows)
            .map(|row| heavy_vector(row as u64, dimension))
            .collect();
        let truth: Vec<Vec<u64>> = queries
            .iter()
            .map(|query| brute_top_k(&vectors, query, 10))
            .collect();
        let recall_at = |ann_rows: &[Vec<u64>]| -> f64 {
            let mut total = 0.0_f64;
            for (ann, truth_rows) in ann_rows.iter().zip(&truth) {
                let overlap = ann.iter().filter(|row| truth_rows.contains(row)).count();
                total += overlap as f64 / ann.len() as f64;
            }
            total / ann_rows.len() as f64
        };
        let collect_rows = |ef: usize| -> Vec<Vec<u64>> {
            queries
                .iter()
                .map(|query| {
                    let hits = ns
                        .search()
                        .vector(query)
                        .top_k(10)
                        .ef(ef)
                        .execute()
                        .expect("search");
                    assert_eq!(hits.len(), 10);
                    hits.iter().map(|hit| hit.rowid.get()).collect()
                })
                .collect()
        };
        let mut curve = format!("ef=128 {:.4}", recall_at(&ann_rows));
        for ef in [256_usize, 512] {
            curve.push_str(&format!("、ef={ef} {:.4}", recall_at(&collect_rows(ef))));
        }
        eprintln!(
            "冒烟规模 {rows}×{dimension}:Recall@10 {curve}(精确参考 {SAMPLES} 条自生成查询,同分按 RowId 升序;均匀随机分布是 HNSW 最坏情形,正式召回门槛由 tests/hnsw_contracts.rs 微缩双分布验收)"
        );
    }
    db.close().expect("close");
}

/// 精确参考 top-k:与引擎同口径的 f32 点积(`Metric::Dot`),同分按 `RowId` 升序。
///
/// 只服务冒烟规模的全方位观测:全量打点成本 $O(\text{rows} \cdot d)$;正式规模
/// 的召回门槛由 `tests/hnsw_contracts.rs` 以微缩双分布验收。
fn brute_top_k(vectors: &[Vec<f32>], query: &[f32], k: usize) -> Vec<u64> {
    let mut scored: Vec<(f32, u64)> = vectors
        .iter()
        .enumerate()
        .map(|(row, vector)| (mneme::simd::dot(query, vector), row as u64))
        .collect();
    scored.sort_by(|left, right| right.0.total_cmp(&left.0).then(left.1.cmp(&right.1)));
    scored.truncate(k);
    scored.into_iter().map(|(_, row)| row).collect()
}
