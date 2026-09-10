//! L3 索引层契约验收测试。
//!
//! 覆盖 `FC-INDEX-PRE-001`(参数校验)、`FC-INDEX-POST-001`(过滤三档与候选暴力等价,
//! 档③恒精确)、`FC-INDEX-POST-002`(`ef→∞` 收敛)、`FC-INDEX-POST-005`(alive/过滤约束)、
//! `FC-INDEX-POST-008`(hidx 持久化与重开载入)、`FC-INDEX-POST-009`(ANN 召回门槛)、
//! `FC-INDEX-INV-008`(前缀 ANN 与未落盘尾部暴力归并),
//! 见 `docs/spec/contracts.md` §3。文件头引用的 `FC-*` 必须与契约矩阵中引用本文件的
//! 条目双向相等,由 `tests/contract_traceability.rs` 机械校验。
//!
//! 覆盖的契约:`FC-INDEX-PRE-001`、`FC-INDEX-POST-001`、`FC-INDEX-POST-002`、
//! `FC-INDEX-POST-005`、`FC-INDEX-POST-008`、`FC-INDEX-POST-009`、`FC-INDEX-INV-008`。

mod common;

use std::collections::HashSet;

use mneme::{Builder, Expr, HnswParams, Metric, Mneme, MnemeError, Record, Tuning, simd};

/// `Record::importance` 越低越易被过滤测试使用。
const RECALL_ROWS: usize = 2_500;
/// 召回测试维度。
const RECALL_DIM: usize = 32;
/// 召回查询数。
const RECALL_QUERIES: usize = 20;

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

/// 召回测试的小参数 HNSW(加快建图,保持 ef=128 门槛)。
fn fast_hnsw() -> HnswParams {
    HnswParams {
        m: 8,
        m0: 16,
        ef_construction: 48,
        ef_search: 64,
    }
}

/// 建一个已 flush(已建 HNSW)的持久库,节点 `RowId` = 插入下标。
fn build_indexed(rows: usize, dim: usize) -> (tempfile::TempDir, Vec<Vec<f32>>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(dim as u32)
        .metric(Metric::Dot)
        .hnsw(fast_hnsw())
        .build()
        .expect("build");
    let ns = db.namespace("t");
    let vectors: Vec<Vec<f32>> = (0..rows).map(|row| vector(row as u64, dim)).collect();
    let batch: Vec<Record> = vectors.iter().map(|v| Record::new(v.clone())).collect();
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");
    db.close().expect("close");
    (dir, vectors)
}

/// 暴力 top-k 的行下标(点积降序、同分升序)。
fn brute_topk(vectors: &[Vec<f32>], query: &[f32], k: usize) -> Vec<usize> {
    let mut scored: Vec<(f32, usize)> = vectors
        .iter()
        .enumerate()
        .map(|(row, v)| (simd::dot(query, v), row))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).expect("finite").then(a.1.cmp(&b.1)));
    scored.truncate(k);
    scored.into_iter().map(|(_, row)| row).collect()
}

/// FC-INDEX-PRE-001:HNSW 参数与过滤阈值非法时建库即拒绝(不静默)。
#[test]
fn invalid_hnsw_params_and_thresholds_are_rejected() {
    // 度数超硬上限 → LimitExceeded。
    let too_wide = Builder::default()
        .dimension(4)
        .hnsw(HnswParams {
            m: 2,
            m0: 9_999,
            ef_construction: 1,
            ef_search: 1,
        })
        .build();
    assert!(matches!(too_wide, Err(MnemeError::LimitExceeded { .. })));

    // m < 2 → Config。
    let too_narrow = Builder::default()
        .dimension(4)
        .hnsw(HnswParams {
            m: 1,
            m0: 1,
            ..HnswParams::default()
        })
        .build();
    assert!(matches!(too_narrow, Err(MnemeError::Config { .. })));

    // 过滤阈值非有限值 → Config。
    let nan_threshold = Builder::default()
        .dimension(4)
        .tuning(Tuning {
            filter_post_threshold: f32::NAN,
            ..Tuning::default()
        })
        .build();
    assert!(matches!(nan_threshold, Err(MnemeError::Config { .. })));

    // 合法参数正常建库。
    assert!(
        Builder::default()
            .dimension(4)
            .hnsw(HnswParams {
                m: 8,
                m0: 16,
                ef_construction: 32,
                ef_search: 64,
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

    let mut all = vectors.clone();
    all.extend(tail);

    let query = vector(4_242, RECALL_DIM);
    let hits = ns
        .search()
        .vector(&query)
        .top_k(10)
        .ef(4096)
        .execute()
        .expect("search");
    let got: HashSet<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
    let want: HashSet<usize> = brute_topk(&all, &query, 10).into_iter().collect();
    assert_eq!(got, want, "前缀 ANN + 尾部暴力应与全量暴力一致");
}

/// FC-INDEX-POST-009:随机均匀数据上 Recall@10(ef=128)≥ 0.95。
#[test]
fn ann_recall_at_ten_meets_threshold() {
    let (dir, vectors) = build_indexed(RECALL_ROWS, RECALL_DIM);
    let db = Mneme::open(dir.path()).expect("open");
    let ns = db.namespace("t");
    let mut hit_sum = 0.0_f64;
    for query_index in 0..RECALL_QUERIES {
        let query = vector(1_000_000 + query_index as u64, RECALL_DIM);
        let hits = ns
            .search()
            .vector(&query)
            .top_k(10)
            .ef(128)
            .execute()
            .expect("search");
        let got: HashSet<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
        let want: HashSet<usize> = brute_topk(&vectors, &query, 10).into_iter().collect();
        hit_sum += got.intersection(&want).count() as f64 / 10.0;
    }
    let recall = hit_sum / RECALL_QUERIES as f64;
    assert!(recall >= 0.95, "Recall@10 未达标:{recall}");
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

/// FC-INDEX-POST-001:极低选择性走档③候选暴力,结果与候选位图内暴力集合相等。
#[test]
fn filter_tier_three_matches_candidate_bruteforce() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(8)
        .metric(Metric::Dot)
        .hnsw(fast_hnsw())
        .build()
        .expect("build");
    let ns = db.namespace("t");
    let batch: Vec<Record> = (0..RECALL_ROWS)
        .map(|row| {
            let rec = Record::new(vector(row as u64, 8));
            if row == 7 {
                rec.metadata(mneme::json!({"tag": "rare"}))
            } else {
                rec.metadata(mneme::json!({"tag": "common"}))
            }
        })
        .collect();
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");

    let query = vector(42, 8);
    let filter = Expr::field("tag").eq("rare");
    let hits = ns
        .search()
        .vector(&query)
        .top_k(5)
        .ef(64)
        .filter(filter)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 1, "稀有标签应恰有一条候选");
    assert_eq!(hits[0].rowid.get(), 7);
}

/// FC-INDEX-POST-001:选择性约 0.5 走档①后过滤,与候选暴力统计等价(召回 ≥ 0.95)。
#[test]
fn filter_post_tier_matches_candidate_bruteforce() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(8)
        .metric(Metric::Dot)
        .hnsw(fast_hnsw())
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

    let query = vector(31_337, 8);
    let hits = ns
        .search()
        .vector(&query)
        .top_k(10)
        .ef(128)
        .filter(Expr::field("tag").eq("even"))
        .execute()
        .expect("search");

    // 候选暴力参照:偶数行内的点积 top-10。
    let mut want: Vec<(f32, usize)> = vectors
        .iter()
        .enumerate()
        .filter(|(row, _)| row % 2 == 0)
        .map(|(row, v)| (simd::dot(&query, v), row))
        .collect();
    want.sort_by(|a, b| b.0.partial_cmp(&a.0).expect("finite").then(a.1.cmp(&b.1)));
    want.truncate(10);
    let want: HashSet<usize> = want.into_iter().map(|(_, row)| row).collect();
    let got: HashSet<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
    let recall = got.intersection(&want).count() as f64 / 10.0;
    assert!(recall >= 0.95, "档①后过滤召回未达标:{recall}");
}

/// FC-INDEX-POST-005:被墓碑遮蔽的记录不出现在 ANN 结果中。
#[test]
fn ann_excludes_deleted_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .path(dir.path())
        .dimension(8)
        .metric(Metric::Dot)
        .hnsw(fast_hnsw())
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
        assert!(ns.delete_by_rowid(mneme::RowId::new(row)).expect("delete"));
        deleted.insert(row);
    }
    let query = vector(999, 8);
    let hits = ns
        .search()
        .vector(&query)
        .top_k(256)
        .ef(128)
        .execute()
        .expect("search");
    for hit in hits {
        assert!(!deleted.contains(&hit.rowid.get()), "命中已删除记录");
    }
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
        stats.segments[0].index_levels >= 1,
        "载入的索引应有上层结构"
    );

    let ns = db.namespace("t");
    let query = vector(7_777_777, RECALL_DIM);
    let hits = ns
        .search()
        .vector(&query)
        .top_k(10)
        .ef(128)
        .execute()
        .expect("search");
    let got: HashSet<usize> = hits.iter().map(|hit| hit.rowid.get() as usize).collect();
    let want: HashSet<usize> = brute_topk(&vectors, &query, 10).into_iter().collect();
    assert!(
        got.intersection(&want).count() >= 8,
        "重开后 ANN 召回明显退化:{got:?} vs {want:?}"
    );
}
