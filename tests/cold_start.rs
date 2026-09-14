//! L2 冷启动门槛(heavy 档,FC-PERSIST-POST-013)。
//!
//! 已 flush 的持久库重开(`Builder::build`)必须 < 1s:打开只读头部与
//! `node_table`,向量/量化码/邻接按需缺页(段句柄惰性驻留,FC-PERSIST-INV-021)。
//! 规模由 `MNEME_HEAVY_ROWS`/`MNEME_HEAVY_DIM` 配置:CI heavy 档显式以
//! 1M×1536 运行(正式门槛),默认 50_000×128 供本地/小 runner 快速冒烟。
//! 默认 `#[ignore]`;heavy CI 用
//! `MNEME_HEAVY=1 cargo test --test cold_start -- --ignored` 执行;
//! `MNEME_HEAVY` 未设置时显式失败,绝不静默跳过(配错 env 的 CI 不得假绿)。
//! 本文件锚定 `FC-PERSIST-POST-013`,无独立 I 编号(I1–I30 自持久化层起)。

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use mneme::{Builder, FsyncPolicy, Metric, Record};

use common::{heavy_dimension, heavy_rows, heavy_vector};

/// 单批写入条数(控制建库期内存峰值)。
const BATCH: usize = 10_000;

/// 建指定规模的持久库并 flush(建索引)后关闭。
///
/// 建库准备期不启动后台维护(`maintenance(false)`):自动 compaction 会与导入
/// 争抢 CPU/IO(500k×1536 实测造成分钟级停顿),而本用例只测量"已 flush 库的
/// 冷启动",维护成本属无关噪声。
fn build_fixture(dir: &Path, rows: usize, dimension: usize) {
    let db = Builder::default()
        .dimension(dimension as u32)
        .metric(Metric::Dot)
        .fsync(FsyncPolicy::OnFlush)
        .maintenance(false)
        .path(dir)
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
    db.flush().expect("flush");
    db.close().expect("close");
}

/// **FC-PERSIST-POST-013**:冷启动 < 1s,且打开后点读/检索仍正确。
///
/// 规模见文件头:正式门槛为 CI 显式 1M×1536,默认小规模冒烟。
#[test]
#[ignore = "heavy:需显式 MNEME_HEAVY=1;规模由 MNEME_HEAVY_ROWS/MNEME_HEAVY_DIM 配置"]
fn cold_open_under_one_second() {
    if std::env::var("MNEME_HEAVY").as_deref() != Ok("1") {
        panic!("MNEME_HEAVY=1 未设置:heavy 门槛不应静默跳过(显式失败,杜绝 CI 假绿)");
    }
    let rows = heavy_rows();
    let dimension = heavy_dimension();
    let dir = tempfile::tempdir().expect("tempdir");
    build_fixture(dir.path(), rows, dimension);

    let started = Instant::now();
    let db = Builder::default()
        .path(dir.path())
        .build()
        .expect("cold open");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "规模 {rows}×{dimension} 冷启动耗时 {elapsed:?} 超过 1s 门槛"
    );

    let ns = db.namespace("heavy");
    let record = ns.get("k0").expect("get").expect("记录存在");
    assert_eq!(record.vector().len(), dimension);
    let hits = ns
        .search()
        .vector(&heavy_vector(rows as u64 + 1, dimension))
        .top_k(10)
        .ef(128)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 10);
    db.close().expect("close");
}
