//! L3 索引层契约验收测试。
//!
//! 覆盖 `FC-INDEX-PRE-001`(参数校验)、`FC-INDEX-POST-001`(过滤三档与候选暴力等价,
//! 档③恒精确)、`FC-INDEX-POST-002`(`ef→∞` 收敛)、`FC-INDEX-POST-005`(alive/过滤约束,
//! 含 `as_of` 历史视图)、`FC-INDEX-POST-008`(hidx 持久化与重开载入,含非恒等重排映射)、
//! `FC-INDEX-POST-009`(ANN 召回门槛,双分布)、`FC-INDEX-INV-008`(前缀 ANN 与未落盘尾部
//! 暴力归并),见 `docs/spec/contracts.md` §3。文件头引用的 `FC-*` 必须与契约矩阵中引用
//! 本文件的条目双向相等,由 `tests/contract_traceability.rs` 机械校验。
//!
//! 覆盖的契约:`FC-INDEX-PRE-001`、`FC-INDEX-POST-001`、`FC-INDEX-POST-002`、
//! `FC-INDEX-POST-005`、`FC-INDEX-POST-008`、`FC-INDEX-POST-009`、`FC-INDEX-INV-008`。

mod common;

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mneme::{Builder, Expr, HnswParams, Metric, Mneme, MnemeError, Record, RowId, Tuning, simd};

/// 过滤与重开测试的行数。
const RECALL_ROWS: usize = 2_500;
/// 召回测试维度。
const RECALL_DIM: usize = 32;
/// 召回查询数(提高统计置信度:个别查询全 0 时仍可能高于门槛,必须降低偶然通过率)。
const RECALL_QUERIES: usize = 50;
/// 档②(放大后过滤)测试行数:候选数 = 20480 / 20 = 1024 = `max(ef, 1024)` 边界。
const AMPLIFIED_TIER_ROWS: usize = 20_480;
/// 档②测试每 `AMPLIFIED_STRIDE` 行打一个稀有标签。
const AMPLIFIED_STRIDE: usize = 20;
/// 档②/档① 过滤测试的查询数。
const FILTER_QUERIES: usize = 20;

/// 确定性伪随机向量(线性同余,避免依赖 `rand`)。
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

/// 8 簇高斯式样本:质心 + 小幅确定性扰动(近似高斯簇,无需 `rand`)。
fn clustered_point(seed: u64, dim: usize) -> Vec<f32> {
    let centers: Vec<Vec<f32>> = (0..8).map(|cluster| vector(9_000 + cluster, dim)).collect();
    let center = &centers[(seed % centers.len() as u64) as usize];
    let noise = vector(seed.wrapping_mul(31).wrapping_add(7), dim);
    (0..dim).map(|col| center[col] + 0.1 * noise[col]).collect()
}

/// 召回测试的小参数 HNSW(加快建图,用于不针对默认参数的用例)。
fn fast_hnsw() -> HnswParams {
    HnswParams {
        m: 8,
        m0: 16,
        ef_construction: 48,
        ef_search: 64,
    }
}

/// 显式钉住"走图"阈值:避免默认 `brute_force_max_rows` 将来上调后,
/// 召回/过滤测试静默退化为暴力扫描而失去证伪力。
fn ann_tuning() -> Tuning {
    Tuning {
        brute_force_max_rows: 100,
        ..Tuning::default()
    }
}

/// 以给定向量与参数建持久库、flush 并关闭;返回临时目录。
fn build_indexed_vectors(
    vectors: &[Vec<f32>],
    dimension: u32,
    hnsw: HnswParams,
    tuning: Tuning,
) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(dimension)
        .metric(Metric::Dot)
        .hnsw(hnsw)
        .tuning(tuning)
        .build()
        .expect("build");
    let ns = db.namespace("t");
    let batch: Vec<Record> = vectors.iter().map(|v| Record::new(v.clone())).collect();
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");
    db.close().expect("close");
    dir
}

/// 建一个已 flush(已建 HNSW)的持久库,节点 `RowId` = 插入下标。
fn build_indexed(rows: usize, dim: usize) -> (tempfile::TempDir, Vec<Vec<f32>>) {
    let vectors: Vec<Vec<f32>> = (0..rows).map(|row| vector(row as u64, dim)).collect();
    let dir = build_indexed_vectors(&vectors, dim as u32, fast_hnsw(), ann_tuning());
    (dir, vectors)
}

/// 暴力 top-k 的行下标(点积降序、同分升序)。
fn brute_topk(vectors: &[Vec<f32>], query: &[f32], k: usize) -> Vec<usize> {
    brute_topk_filtered(vectors, query, k, |_| true)
}

/// 在候选子集内暴力 top-k(过滤三档的精确参照)。
fn brute_topk_filtered(
    vectors: &[Vec<f32>],
    query: &[f32],
    k: usize,
    keep: impl Fn(usize) -> bool,
) -> Vec<usize> {
    let mut scored: Vec<(f32, usize)> = vectors
        .iter()
        .enumerate()
        .filter(|(row, _)| keep(*row))
        .map(|(row, v)| (simd::dot(query, v), row))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).expect("finite").then(a.1.cmp(&b.1)));
    scored.truncate(k);
    scored.into_iter().map(|(_, row)| row).collect()
}

/// 在一份已 flush 的库上跑多查询,返回平均 Recall@10。
fn average_recall(dir: &Path, vectors: &[Vec<f32>], queries: &[Vec<f32>], ef: usize) -> f64 {
    let db = Mneme::open(dir).expect("open");
    let ns = db.namespace("t");
    let mut hit_sum = 0.0;
    for query in queries {
        let hits = ns
            .search()
            .vector(query)
            .top_k(10)
            .ef(ef)
            .execute()
            .expect("search");
        let got: HashSet<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
        let want: HashSet<usize> = brute_topk(vectors, query, 10).into_iter().collect();
        hit_sum += got.intersection(&want).count() as f64 / 10.0;
    }
    hit_sum / queries.len() as f64
}

/// FC-INDEX-PRE-001:HNSW 参数与过滤阈值非法时建库即拒绝(不静默),覆盖全部前置边界。
#[test]
fn invalid_hnsw_params_and_thresholds_are_rejected() {
    // 度数超硬上限 → LimitExceeded(字段与上界、实参均须准确)。
    assert!(matches!(
        Builder::default()
            .dimension(4)
            .hnsw(HnswParams {
                m: 2,
                m0: 9_999,
                ef_construction: 1,
                ef_search: 1,
            })
            .build(),
        Err(MnemeError::LimitExceeded {
            field: "hnsw 度数(m/m0)",
            limit: 4096,
            got: 9_999,
        })
    ));

    // m < 2 → Config。
    assert!(matches!(
        Builder::default()
            .dimension(4)
            .hnsw(HnswParams {
                m: 1,
                m0: 1,
                ..HnswParams::default()
            })
            .build(),
        Err(MnemeError::Config { reason }) if reason.contains("m") && reason.contains("m0")
    ));

    // m0 < m(仅此项违反,证明校验分支独立于 m<2)。
    assert!(matches!(
        Builder::default()
            .dimension(4)
            .hnsw(HnswParams {
                m: 4,
                m0: 3,
                ..HnswParams::default()
            })
            .build(),
        Err(MnemeError::Config { reason }) if reason.contains("m0")
    ));

    // ef_construction = 0 → Config(下界紧邻越界点;=1 为合法边界,见末尾)。
    assert!(matches!(
        Builder::default()
            .dimension(4)
            .hnsw(HnswParams {
                ef_construction: 0,
                ..HnswParams::default()
            })
            .build(),
        Err(MnemeError::Config { reason }) if reason.contains("ef_construction")
    ));

    // ef_search = 0 → Config;ef_search > Limits.ef_max → LimitExceeded
    // (否则默认查询宽度可绕过查询期上限)。
    assert!(matches!(
        Builder::default()
            .dimension(4)
            .hnsw(HnswParams {
                ef_search: 0,
                ..HnswParams::default()
            })
            .build(),
        Err(MnemeError::Config { reason }) if reason.contains("ef_search")
    ));
    assert!(matches!(
        Builder::default()
            .dimension(4)
            .hnsw(HnswParams {
                ef_search: 4_097,
                ..HnswParams::default()
            })
            .build(),
        Err(MnemeError::LimitExceeded {
            field: "ef_search",
            limit: 4096,
            got: 4_097,
        })
    ));

    // 过滤阈值:非有限值、越界、brute > post 各自拒绝。
    assert!(matches!(
        Builder::default()
            .dimension(4)
            .tuning(Tuning {
                filter_post_threshold: f32::NAN,
                ..Tuning::default()
            })
            .build(),
        Err(MnemeError::Config { reason }) if reason.contains("过滤")
    ));
    assert!(matches!(
        Builder::default()
            .dimension(4)
            .tuning(Tuning {
                filter_post_threshold: 1.5,
                ..Tuning::default()
            })
            .build(),
        Err(MnemeError::Config { .. })
    ));
    assert!(matches!(
        Builder::default()
            .dimension(4)
            .tuning(Tuning {
                filter_brute_threshold: -0.1,
                ..Tuning::default()
            })
            .build(),
        Err(MnemeError::Config { .. })
    ));
    assert!(matches!(
        Builder::default()
            .dimension(4)
            .tuning(Tuning {
                filter_brute_threshold: 0.5,
                filter_post_threshold: 0.1,
                ..Tuning::default()
            })
            .build(),
        Err(MnemeError::Config { reason }) if reason.contains("brute")
    ));

    // 合法边界正常建库:m=2/m0=2、ef_construction=1、ef_search=1;m=m0=4096 硬上限亦合法。
    assert!(
        Builder::default()
            .dimension(4)
            .hnsw(HnswParams {
                m: 2,
                m0: 2,
                ef_construction: 1,
                ef_search: 1,
            })
            .build()
            .is_ok()
    );
    // 阈值合法边界:post=brute=1.0 与 post=brute=0.0 均合法。
    assert!(
        Builder::default()
            .dimension(4)
            .tuning(Tuning {
                filter_post_threshold: 1.0,
                filter_brute_threshold: 1.0,
                ..Tuning::default()
            })
            .build()
            .is_ok()
    );
    assert!(
        Builder::default()
            .dimension(4)
            .tuning(Tuning {
                filter_post_threshold: 0.0,
                filter_brute_threshold: 0.0,
                ..Tuning::default()
            })
            .build()
            .is_ok()
    );
    assert!(
        Builder::default()
            .dimension(4)
            .hnsw(HnswParams {
                m: 4_096,
                m0: 4_096,
                ef_construction: 1,
                ef_search: 4_096,
            })
            .build()
            .is_ok()
    );
}

/// FC-INDEX-INV-008:索引前缀 ANN 与未落盘尾部暴力归并结果 ≡ 全量候选暴力(ef→∞)。
#[test]
fn ann_merges_prefix_with_unflushed_tail() {
    let (dir, vectors) = build_indexed(RECALL_ROWS, RECALL_DIM);
    let db = Mneme::open(dir.path()).expect("open");
    let ns = db.namespace("t");
    // 追加未 flush 的尾部记录:不在 hidx 图中,只能由尾部暴力扫描覆盖。
    let tail: Vec<Vec<f32>> = (0..300)
        .map(|row| vector(5_000_000 + row as u64, RECALL_DIM))
        .collect();
    let batch: Vec<Record> = tail.iter().map(|v| Record::new(v.clone())).collect();
    ns.insert_batch(batch).expect("tail insert");

    let query = tail[0].clone();
    let mut all = vectors.clone();
    all.extend(tail);

    // 查询直接取一条尾部向量:保证全量 top-10 必含尾部行,否则"实现只返回索引
    // 前缀、丢弃尾部"的退化也能通过,归并路径失去证伪力。
    let want_rows = brute_topk(&all, &query, 10);
    assert!(
        want_rows.iter().any(|&row| row >= RECALL_ROWS),
        "测试数据必须让尾部行进入全量 top-10,否则归并无证伪力"
    );
    let hits = ns
        .search()
        .vector(&query)
        .top_k(10)
        .ef(4096)
        .execute()
        .expect("search");
    let got: HashSet<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
    let want: HashSet<usize> = want_rows.into_iter().collect();
    assert_eq!(got, want, "前缀 ANN + 尾部暴力应与全量暴力一致");
}

/// FC-INDEX-POST-009:默认 HNSW 参数下,均匀与簇状两种分布的 Recall@10(ef=128)≥ 0.95。
#[test]
fn ann_recall_at_ten_meets_threshold() {
    // 分布一:随机均匀。
    let uniform: Vec<Vec<f32>> = (0..RECALL_ROWS)
        .map(|row| vector(row as u64, RECALL_DIM))
        .collect();
    let dir = build_indexed_vectors(
        &uniform,
        RECALL_DIM as u32,
        HnswParams::default(),
        ann_tuning(),
    );
    let queries: Vec<Vec<f32>> = (0..RECALL_QUERIES)
        .map(|index| vector(1_000_000 + index as u64, RECALL_DIM))
        .collect();
    let recall = average_recall(dir.path(), &uniform, &queries, 128);
    assert!(recall >= 0.95, "均匀数据 Recall@10 未达标:{recall}");

    // 分布二:8 簇高斯(确定性构造)。
    let clustered: Vec<Vec<f32>> = (0..RECALL_ROWS)
        .map(|row| clustered_point(row as u64, RECALL_DIM))
        .collect();
    let dir = build_indexed_vectors(
        &clustered,
        RECALL_DIM as u32,
        HnswParams::default(),
        ann_tuning(),
    );
    let queries: Vec<Vec<f32>> = (0..RECALL_QUERIES)
        .map(|index| clustered_point(1_000_000 + index as u64, RECALL_DIM))
        .collect();
    let recall = average_recall(dir.path(), &clustered, &queries, 128);
    assert!(recall >= 0.95, "簇状数据 Recall@10 未达标:{recall}");
}

/// FC-INDEX-POST-002:`ef → ∞` 时 ANN 结果收敛于精确暴力。
#[test]
fn ann_converges_to_bruteforce_with_large_ef() {
    let (dir, vectors) = build_indexed(RECALL_ROWS, RECALL_DIM);
    let db = Mneme::open(dir.path()).expect("open");
    let ns = db.namespace("t");
    for query_index in 0..5 {
        let query = vector(2_000_000 + query_index as u64, RECALL_DIM);
        let hits = ns
            .search()
            .vector(&query)
            .top_k(10)
            .ef(4096)
            .execute()
            .expect("search");
        let got: HashSet<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
        let want: HashSet<usize> = brute_topk(&vectors, &query, 10).into_iter().collect();
        assert_eq!(got, want, "ef 极大时应与暴力精确一致");
    }
}

/// FC-INDEX-POST-001:候选数 < `max(ef,1024)`(且选择性 > 阈值)走档③候选暴力,
/// 多个稀有候选下与候选位图内暴力集合与次序完全一致。
#[test]
fn filter_tier_three_matches_candidate_bruteforce() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(8)
        .metric(Metric::Dot)
        .hnsw(fast_hnsw())
        .tuning(ann_tuning())
        .build()
        .expect("build");
    let ns = db.namespace("t");
    let vectors: Vec<Vec<f32>> = (0..RECALL_ROWS).map(|row| vector(row as u64, 8)).collect();
    let batch: Vec<Record> = vectors
        .iter()
        .enumerate()
        .map(|(row, v)| {
            let rec = Record::new(v.clone());
            if row % 250 == 0 {
                rec.metadata(mneme::json!({"tag": "rare"}))
            } else {
                rec.metadata(mneme::json!({"tag": "common"}))
            }
        })
        .collect();
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");

    // 10 个稀有候选:选择性 0.004 > brute 阈值,但候选数 10 < max(ef,1024) → 档③。
    let query = vector(42, 8);
    let hits = ns
        .search()
        .vector(&query)
        .top_k(5)
        .ef(64)
        .filter(Expr::field("tag").eq("rare"))
        .execute()
        .expect("search");
    let got: Vec<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
    let want = brute_topk_filtered(&vectors, &query, 5, |row| row % 250 == 0);
    assert_eq!(got, want, "档③必须与候选位图内暴力集合与次序一致");
}

/// FC-INDEX-POST-001:档③的两个触发条件(选择性 ≤ `brute`;候选数 < `max(ef,1024)`)
/// 各自独立成立,且均与候选暴力精确一致。
#[test]
fn filter_brute_trigger_conditions_are_independent() {
    // 数据:2500 行,偶数行打 "even"(选择性 0.5)。
    let vectors: Vec<Vec<f32>> = (0..RECALL_ROWS).map(|row| vector(row as u64, 8)).collect();
    let batch: Vec<Record> = vectors
        .iter()
        .enumerate()
        .map(|(row, v)| {
            let rec = Record::new(v.clone());
            if row % 2 == 0 {
                rec.metadata(mneme::json!({"tag": "even"}))
            } else {
                rec.metadata(mneme::json!({"tag": "odd"}))
            }
        })
        .collect();

    // 触发条件一:s=0.5 ≤ brute=0.6(候选数 1250 ≥ max(ef=128,1024))→ 档③。
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(8)
        .metric(Metric::Dot)
        .hnsw(fast_hnsw())
        .tuning(Tuning {
            brute_force_max_rows: 100,
            filter_brute_threshold: 0.6,
            filter_post_threshold: 0.9,
            ..Tuning::default()
        })
        .build()
        .expect("build");
    let ns = db.namespace("t");
    ns.insert_batch(batch.clone()).expect("insert_batch");
    db.flush().expect("flush");
    let query = vector(8_888, 8);
    let hits = ns
        .search()
        .vector(&query)
        .top_k(10)
        .ef(128)
        .filter(Expr::field("tag").eq("even"))
        .execute()
        .expect("search");
    let got: Vec<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
    let want = brute_topk_filtered(&vectors, &query, 10, |row| row % 2 == 0);
    assert_eq!(got, want, "选择性触发档③时应精确等于候选暴力");
    db.close().expect("close");

    // 触发条件二:s=0.5 > brute=0.1,但候选数 1250 < max(ef=2048,1024)→ 档③。
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(8)
        .metric(Metric::Dot)
        .hnsw(fast_hnsw())
        .tuning(Tuning {
            brute_force_max_rows: 100,
            filter_brute_threshold: 0.1,
            filter_post_threshold: 0.9,
            ..Tuning::default()
        })
        .build()
        .expect("build");
    let ns = db.namespace("t");
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");
    let hits = ns
        .search()
        .vector(&query)
        .top_k(10)
        .ef(2048)
        .filter(Expr::field("tag").eq("even"))
        .execute()
        .expect("search");
    let got: Vec<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
    let want = brute_topk_filtered(&vectors, &query, 10, |row| row % 2 == 0);
    assert_eq!(got, want, "候选数触发档③时应精确等于候选暴力");
    db.close().expect("close");
}

/// FC-INDEX-POST-001:选择性约 0.5 走档①后过滤,与候选暴力统计等价(多查询召回 ≥ 0.95)。
#[test]
fn filter_post_tier_matches_candidate_bruteforce() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(8)
        .metric(Metric::Dot)
        .hnsw(fast_hnsw())
        .tuning(ann_tuning())
        .build()
        .expect("build");
    let ns = db.namespace("t");
    let vectors: Vec<Vec<f32>> = (0..RECALL_ROWS).map(|row| vector(row as u64, 8)).collect();
    let batch: Vec<Record> = vectors
        .iter()
        .enumerate()
        .map(|(row, v)| {
            let rec = Record::new(v.clone());
            if row % 2 == 0 {
                rec.metadata(mneme::json!({"tag": "even"}))
            } else {
                rec.metadata(mneme::json!({"tag": "odd"}))
            }
        })
        .collect();
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");

    let mut hit_sum = 0.0;
    for query_index in 0..FILTER_QUERIES {
        let query = vector(31_337 + query_index as u64, 8);
        let hits = ns
            .search()
            .vector(&query)
            .top_k(10)
            .ef(128)
            .filter(Expr::field("tag").eq("even"))
            .execute()
            .expect("search");
        let got: HashSet<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
        let want: HashSet<usize> = brute_topk_filtered(&vectors, &query, 10, |row| row % 2 == 0)
            .into_iter()
            .collect();
        hit_sum += got.intersection(&want).count() as f64 / 10.0;
    }
    let recall = hit_sum / FILTER_QUERIES as f64;
    assert!(recall >= 0.95, "档①后过滤多查询召回未达标:{recall}");
}

/// FC-INDEX-POST-001:选择性 0.05、候选数 1024 = `max(ef,1024)` 边界走档②
/// (全图遍历 + `ef×4` 后过滤),与候选暴力统计等价(多查询召回 ≥ 0.95)。
#[test]
fn filter_amplified_tier_matches_candidate_bruteforce() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(8)
        .metric(Metric::Dot)
        .hnsw(fast_hnsw())
        .tuning(ann_tuning())
        .build()
        .expect("build");
    let ns = db.namespace("t");
    let vectors: Vec<Vec<f32>> = (0..AMPLIFIED_TIER_ROWS)
        .map(|row| vector(row as u64, 8))
        .collect();
    let batch: Vec<Record> = vectors
        .iter()
        .enumerate()
        .map(|(row, v)| {
            let rec = Record::new(v.clone());
            if row % AMPLIFIED_STRIDE == 0 {
                rec.metadata(mneme::json!({"tag": "rare"}))
            } else {
                rec.metadata(mneme::json!({"tag": "common"}))
            }
        })
        .collect();
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");

    // 候选数 = 20480/20 = 1024,s = 0.05 ∈ (0.001, 0.10] 且候选数 = max(ef=512,1024) → 档②;
    // ef' = max(512,10)×4 = 2048,满足后过滤统计等价口径 `ef'·s ≳ 4k`(2048×0.05 ≈ 102 ≥ 40)。
    let mut hit_sum = 0.0;
    for query_index in 0..FILTER_QUERIES {
        let query = vector(4_200_000 + query_index as u64, 8);
        let hits = ns
            .search()
            .vector(&query)
            .top_k(10)
            .ef(512)
            .filter(Expr::field("tag").eq("rare"))
            .execute()
            .expect("search");
        let got: HashSet<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
        let want: HashSet<usize> =
            brute_topk_filtered(&vectors, &query, 10, |row| row % AMPLIFIED_STRIDE == 0)
                .into_iter()
                .collect();
        hit_sum += got.intersection(&want).count() as f64 / 10.0;
    }
    let recall = hit_sum / FILTER_QUERIES as f64;
    assert!(recall >= 0.95, "档②放大后过滤多查询召回未达标:{recall}");
}

/// FC-INDEX-POST-005:被墓碑遮蔽的记录不出现在 ANN 结果中,且结果满额不被墓碑挤占。
#[test]
fn ann_excludes_deleted_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(8)
        .metric(Metric::Dot)
        .hnsw(fast_hnsw())
        .tuning(ann_tuning())
        .build()
        .expect("build");
    let ns = db.namespace("t");
    let batch: Vec<Record> = (0..RECALL_ROWS)
        .map(|row| Record::new(vector(row as u64, 8)))
        .collect();
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");

    let mut deleted = HashSet::new();
    for row in 0..100_u64 {
        assert!(ns.delete_by_rowid(RowId::new(row)).expect("delete"));
        deleted.insert(row);
    }
    let query = vector(999, 8);
    let hits = ns
        .search()
        .vector(&query)
        .top_k(256)
        .ef(2048)
        .execute()
        .expect("search");
    assert_eq!(
        hits.len(),
        256,
        "删除 100 行后 top-256 应满额(墓碑不被返回)"
    );
    for hit in hits {
        assert!(!deleted.contains(&hit.rowid.get()), "命中已删除记录");
    }
}

/// 可手动推进的测试时钟(Unix 毫秒),用于确定性 `as_of` 历史验收。
#[derive(Debug)]
struct TestClock(std::sync::atomic::AtomicI64);

impl TestClock {
    fn new(now_ms: i64) -> Self {
        Self(std::sync::atomic::AtomicI64::new(now_ms))
    }

    fn set(&self, now_ms: i64) {
        self.0.store(now_ms, std::sync::atomic::Ordering::Relaxed);
    }
}

impl mneme::Clock for TestClock {
    fn now_unix_ms(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// FC-INDEX-POST-005:`as_of` 历史视图上的 ANN 必须按历史 alive 位图返回已删记录,
/// 当前视图不得返回;两视图分别与同视图暴力一致。
#[test]
fn ann_after_as_of_matches_bruteforce() {
    const T0: i64 = 1_700_000_000_000;
    let deleted_rows = 50_u64;
    let vectors: Vec<Vec<f32>> = (0..RECALL_ROWS)
        .map(|row| vector(row as u64, RECALL_DIM))
        .collect();
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let clock = Arc::new(TestClock::new(T0));
        let db = Builder::default()
            .path(dir.path())
            .dimension(RECALL_DIM as u32)
            .metric(Metric::Dot)
            .hnsw(fast_hnsw())
            .tuning(ann_tuning())
            .clock(Arc::clone(&clock) as Arc<dyn mneme::Clock>)
            .build()
            .expect("build");
        let ns = db.namespace("t");
        let batch: Vec<Record> = vectors.iter().map(|v| Record::new(v.clone())).collect();
        ns.insert_batch(batch).expect("insert_batch");
        // 事务时间推进后再删除:删除版本 tx_ms = T0+1000,历史时点 T0+500 看不到它。
        clock.set(T0 + 1_000);
        for row in 0..deleted_rows {
            assert!(ns.delete_by_rowid(RowId::new(row)).expect("delete"));
        }
        db.flush().expect("flush");
        db.close().expect("close");
    }

    // 重开:版本事务时间随段恢复;查询期 `as_of` 与注入时钟比较。
    let clock = Arc::new(TestClock::new(T0 + 2_000));
    let db = Builder::default()
        .path(dir.path())
        .clock(Arc::clone(&clock) as Arc<dyn mneme::Clock>)
        .build()
        .expect("reopen");
    let ns = db.namespace("t");
    // 查询取已删记录 7 的向量:历史视图必须能返回它(并含全部记录)。
    let query = vectors[7].clone();

    let past = ns
        .search()
        .vector(&query)
        .top_k(RECALL_ROWS)
        .as_of(T0 + 500)
        .ef(4096)
        .execute()
        .expect("search");
    let got_past: HashSet<usize> = past.iter().map(|hit| hit.rowid.get() as usize).collect();
    let want_past: HashSet<usize> = brute_topk(&vectors, &query, RECALL_ROWS)
        .into_iter()
        .collect();
    assert_eq!(got_past, want_past, "as_of 历史视图应与删除前全量暴力一致");
    assert!(got_past.contains(&7), "历史视图必须仍能看到删除前的记录");

    let now = ns
        .search()
        .vector(&query)
        .top_k(RECALL_ROWS)
        .ef(4096)
        .execute()
        .expect("search");
    let got_now: HashSet<usize> = now.iter().map(|hit| hit.rowid.get() as usize).collect();
    let want_now: HashSet<usize> = brute_topk_filtered(&vectors, &query, RECALL_ROWS, |row| {
        row >= deleted_rows as usize
    })
    .into_iter()
    .collect();
    assert_eq!(got_now, want_now, "当前视图应与存活记录暴力一致");
    assert!(!got_now.contains(&7), "当前视图不得返回已删除记录");
}

/// FC-INDEX-POST-005:ANN 前缀的 alive 位图同时按命名空间与 TTL 过滤——其他
/// 命名空间与已逻辑过期记录不得入选,本命名空间内结果与「TTL 过滤后」暴力一致。
#[test]
fn ann_respects_namespace_and_ttl_visibility() {
    const T0: i64 = 1_700_100_000_000;
    const OTHER_ROWS: usize = 100;
    let vectors: Vec<Vec<f32>> = (0..RECALL_ROWS).map(|row| vector(row as u64, 8)).collect();
    // 哨兵向量:放大 1000 倍使其点积碾压普通随机向量。若 TTL 或 NS 过滤失效,
    // 哨兵必进(甚至居首)结果集,断言必然变红——否则测试只是空转。
    let sentinel: Vec<f32> = vectors[0].iter().map(|x| x * 1_000.0).collect();
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = Arc::new(TestClock::new(T0));
    let db = Builder::default()
        .path(dir.path())
        .dimension(8)
        .metric(Metric::Dot)
        .hnsw(fast_hnsw())
        .tuning(ann_tuning())
        .clock(Arc::clone(&clock) as Arc<dyn mneme::Clock>)
        .build()
        .expect("build");
    // 命名空间 a:首行存哨兵且带 1s TTL,其余为普通向量。
    let ns_a = db.namespace("a");
    let mut batch: Vec<Record> = vectors.iter().map(|v| Record::new(v.clone())).collect();
    batch[0] = Record::new(sentinel.clone()).ttl(Duration::from_millis(1_000));
    ns_a.insert_batch(batch).expect("insert a");
    // 命名空间 b:首行存同一哨兵(永久),与 a 同段落盘(hidx 前缀覆盖两 NS)。
    let ns_b = db.namespace("b");
    let mut other: Vec<Record> = (0..OTHER_ROWS)
        .map(|row| Record::new(vector(8_000_000 + row as u64, 8)))
        .collect();
    other[0] = Record::new(sentinel.clone());
    ns_b.insert_batch(other).expect("insert b");
    db.flush().expect("flush");

    // 推进时钟:首行 TTL 到期,alive 位图必须摘除它。
    clock.set(T0 + 2_000);
    let hits = ns_a
        .search()
        .vector(&sentinel)
        .top_k(10)
        .ef(4096)
        .execute()
        .expect("search");
    let got: Vec<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
    assert!(
        !got.contains(&0),
        "已逻辑过期哨兵不得被 ANN 返回(若 TTL 过滤失效它必然居首)"
    );
    assert!(
        got.iter().all(|&row| row < RECALL_ROWS),
        "其他命名空间哨兵不得入选(若 NS 过滤失效它必然居首):{got:?}"
    );
    // a 内 oracle(排除已过期首行):任何可见性泄漏都会破坏集合相等。
    let want: HashSet<usize> = brute_topk_filtered(&vectors, &sentinel, 10, |row| row != 0)
        .into_iter()
        .collect();
    assert_eq!(
        got.into_iter().collect::<HashSet<_>>(),
        want,
        "a 命名空间结果应与 TTL 过滤后暴力一致"
    );
}

/// FC-INDEX-POST-008:hidx 随 flush 落盘并登记入口,重开从 hidx 载入且检索正确。
#[test]
fn reopen_loads_hnsw_from_hidx() {
    let (dir, vectors) = build_indexed(RECALL_ROWS, RECALL_DIM);
    let db = Mneme::open(dir.path()).expect("reopen");
    let stats = db.stats().expect("stats");
    assert_eq!(stats.segments.len(), 1);
    assert_eq!(
        stats.segments[0].index_nodes, RECALL_ROWS as u64,
        "重开后应从 hidx 载入完整索引(而非降级暴力)"
    );
    assert!(
        (1..=8).contains(&stats.segments[0].index_levels),
        "载入的索引应有上层结构且层级在合理范围"
    );

    let ns = db.namespace("t");
    let query = vector(7_777_777, RECALL_DIM);
    let hits = ns
        .search()
        .vector(&query)
        .top_k(10)
        .ef(4096)
        .execute()
        .expect("search");
    let got: HashSet<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
    let want: HashSet<usize> = brute_topk(&vectors, &query, 10).into_iter().collect();
    assert_eq!(got, want, "重开载入的图在 ef→∞ 时应与暴力一致");
}

/// FC-INDEX-POST-008:删除使恢复期"(rowid,seqno) 排序位置"偏离段内槽位次序
/// (非恒等重排映射),重开载入的索引仍须把 hidx 节点正确映射回全局槽位。
#[test]
fn reopen_after_delete_remaps_slots() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vectors: Vec<Vec<f32>> = (0..RECALL_ROWS)
        .map(|row| vector(row as u64, RECALL_DIM))
        .collect();
    let deleted_rows = 300_u64;
    {
        let db = Builder::default()
            .path(dir.path())
            .dimension(RECALL_DIM as u32)
            .metric(Metric::Dot)
            .hnsw(fast_hnsw())
            .tuning(ann_tuning())
            .build()
            .expect("build");
        let ns = db.namespace("t");
        let batch: Vec<Record> = vectors.iter().map(|v| Record::new(v.clone())).collect();
        ns.insert_batch(batch).expect("insert_batch");
        // 删除前 300 行:墓碑版本追加在末尾,使恢复期排序后重排映射非恒等。
        for row in 0..deleted_rows {
            assert!(ns.delete_by_rowid(RowId::new(row)).expect("delete"));
        }
        db.flush().expect("flush");
        db.close().expect("close");
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let stats = db.stats().expect("stats");
    assert_eq!(
        stats.segments[0].index_nodes,
        (RECALL_ROWS as u64) + deleted_rows,
        "索引应覆盖全部物理版本(含墓碑),且非恒等重排后节点数正确"
    );
    let ns = db.namespace("t");
    // 小 ef 多查询:重排映射若写反/恒等,图与向量错配,召回会显著下降。
    // `ef→∞` 会遍历全图,对任意置换不敏感,不能单独作证伪(见下复核)。
    let mut hit_sum = 0.0;
    for query_index in 0..RECALL_QUERIES {
        let query = vector(3_141_592 + query_index as u64, RECALL_DIM);
        let hits = ns
            .search()
            .vector(&query)
            .top_k(10)
            .ef(128)
            .execute()
            .expect("search");
        for hit in &hits {
            assert!(
                hit.rowid.get() >= deleted_rows,
                "重排后不得命中已删除记录:{}",
                hit.rowid.get()
            );
        }
        let got: HashSet<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
        let want: HashSet<usize> =
            brute_topk_filtered(&vectors, &query, 10, |row| row >= deleted_rows as usize)
                .into_iter()
                .collect();
        hit_sum += got.intersection(&want).count() as f64 / 10.0;
    }
    let recall = hit_sum / RECALL_QUERIES as f64;
    assert!(
        recall >= 0.95,
        "非恒等重排映射下 ef=128 召回退化(疑似映射方向错误):{recall}"
    );

    // 另以 ef→∞ 精确复核(映射正确时必然集合相等)。
    let query = vector(2_718_281, RECALL_DIM);
    let hits = ns
        .search()
        .vector(&query)
        .top_k(10)
        .ef(4096)
        .execute()
        .expect("search");
    let got: HashSet<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
    let want: HashSet<usize> =
        brute_topk_filtered(&vectors, &query, 10, |row| row >= deleted_rows as usize)
            .into_iter()
            .collect();
    assert_eq!(got, want, "非恒等重排映射下 ef→∞ 结果应与存活记录暴力一致");
}
