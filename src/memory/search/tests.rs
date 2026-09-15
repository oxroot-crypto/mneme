use super::*;

use crate::core::metric::Metric;
use crate::core::types::{RowId, SlotId};

use super::ann::{COARSE_CANDIDATES, SEGMENT_ALIVE_CACHE_HITS, SEGMENT_ALIVE_CACHE_STORES};
use super::scan::{SCORE_CALLS, compare_scored};

#[test]
fn norm_sq_of_unit_basis_is_one() {
    assert!((norm_sq(&[1.0, 0.0, 0.0]) - 1.0).abs() < 1e-6);
}

/// FC-MEM-CPLX-001(操作计数:扫描打分次数 = 候选数,无隐藏全扫)
#[test]
fn scan_touches_each_candidate_once() {
    let db = crate::memory::Mneme::in_memory(2).expect("in_memory");
    let ns = db.namespace("n");
    for index in 0..8 {
        ns.insert(crate::memory::Record::new(vec![1.0, 0.0]).key(format!("k{index}")))
            .expect("insert");
    }
    SCORE_CALLS.with(|calls| calls.set(0));
    let hits = ns
        .search()
        .vector(&[1.0, 0.0])
        .top_k(4)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 4);
    let calls = SCORE_CALLS.with(std::cell::Cell::get);
    assert_eq!(calls, 8, "扫描打分次数必须等于候选数(线性,无隐藏全扫)");
}

/// FC-INDEX-POST-009(分派):段行数超过 `brute_force_max_rows` 且视图带索引时,
/// 查询必须走索引搜索,不得回退为全量暴力重扫——否则恒暴力退化解无人察觉。
#[test]
fn search_dispatches_to_index_when_prefix_exceeds_brute_threshold() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = crate::memory::Builder::default()
        .path(dir.path())
        .dimension(4)
        .tuning(crate::core::options::Tuning {
            brute_force_max_rows: 8,
            ..crate::core::options::Tuning::default()
        })
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let batch: Vec<crate::memory::Record> = (0..64)
        .map(|row| crate::memory::Record::new(vec![row as f32, 1.0, 0.0, 0.0]))
        .collect();
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");

    SCORE_CALLS.with(|calls| calls.set(0));
    let hits = ns
        .search()
        .vector(&[1.0, 0.0, 0.0, 0.0])
        .top_k(4)
        .ef(16)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 4);
    let calls = SCORE_CALLS.with(std::cell::Cell::get);
    assert_eq!(
        calls, 0,
        "有索引且段行数超过阈值时必须走索引,不得回退暴力重扫前缀"
    );
}

/// FC-QUERY-POST-008:段 alive 位图在同视图内复用(第二次查询命中缓存),
/// 结果不受缓存影响;写事务发布新视图后不复用旧缓存,新行立刻可见。
#[test]
fn segment_alive_cache_is_transparent_and_view_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = crate::memory::Builder::default()
        .path(dir.path())
        .dimension(4)
        .tuning(crate::core::options::Tuning {
            brute_force_max_rows: 8,
            ..crate::core::options::Tuning::default()
        })
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let batch: Vec<crate::memory::Record> = (0..16)
        .map(|row| crate::memory::Record::new(vec![row as f32, 1.0, 0.0, 0.0]))
        .collect();
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");

    SEGMENT_ALIVE_CACHE_HITS.with(|hits| hits.set(0));
    let search = |vector: &[f32]| {
        ns.search()
            .vector(vector)
            .top_k(4)
            .ef(16)
            .execute()
            .expect("search")
    };
    let first: Vec<_> = search(&[1.0, 0.0, 0.0, 0.0])
        .iter()
        .map(|hit| hit.rowid)
        .collect();
    let hits_after_first = SEGMENT_ALIVE_CACHE_HITS.with(std::cell::Cell::get);
    let second: Vec<_> = search(&[1.0, 0.0, 0.0, 0.0])
        .iter()
        .map(|hit| hit.rowid)
        .collect();
    let hits_after_second = SEGMENT_ALIVE_CACHE_HITS.with(std::cell::Cell::get);
    assert_eq!(first, second, "缓存命中不得改变查询结果");
    assert!(
        hits_after_second > hits_after_first,
        "同视图第二次查询必须命中段 alive 位图缓存"
    );

    // 新写入发布新视图:缓存不可跨视图复用,新行必须可见。
    ns.insert(crate::memory::Record::new(vec![0.0, 1.0, 0.0, 0.0]))
        .expect("insert new");
    let third = search(&[0.0, 1.0, 0.0, 0.0]);
    assert_eq!(third.len(), 4);
    assert!(
        third
            .iter()
            .any(|hit| hit.rowid == crate::core::types::RowId::new(16)),
        "新视图必须包含新写入行(缓存不得跨视图复用)"
    );
}

/// FC-QUERY-POST-008:段内存在 TTL 行时不写、不命中 alive 位图缓存
/// (过期随时间实时反映,绝不因缓存复活)。
#[test]
fn segment_alive_cache_skips_ttl_segments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = crate::memory::Builder::default()
        .path(dir.path())
        .dimension(4)
        .tuning(crate::core::options::Tuning {
            brute_force_max_rows: 8,
            ..crate::core::options::Tuning::default()
        })
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let mut batch: Vec<crate::memory::Record> = (0..16)
        .map(|row| crate::memory::Record::new(vec![row as f32, 1.0, 0.0, 0.0]))
        .collect();
    batch.push(
        crate::memory::Record::new(vec![1.0, 1.0, 1.0, 1.0])
            .ttl(std::time::Duration::from_secs(3_600)),
    );
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");

    SEGMENT_ALIVE_CACHE_HITS.with(|hits| hits.set(0));
    SEGMENT_ALIVE_CACHE_STORES.with(|stores| stores.set(0));
    for _ in 0..2 {
        let _ = ns
            .search()
            .vector(&[1.0, 0.0, 0.0, 0.0])
            .top_k(4)
            .ef(16)
            .execute()
            .expect("search");
    }
    assert_eq!(
        SEGMENT_ALIVE_CACHE_STORES.with(std::cell::Cell::get),
        0,
        "含 TTL 段的 alive 位图不得写缓存"
    );
    assert_eq!(
        SEGMENT_ALIVE_CACHE_HITS.with(std::cell::Cell::get),
        0,
        "含 TTL 段不得命中 alive 位图缓存"
    );
}

/// FC-INDEX-POST-009(分派边界):段行数等于 `brute_force_max_rows` 时仍走暴力
/// (契约口径为严格「超过」),且索引存在与否不改变该边界判定。
#[test]
fn search_bruteforces_when_prefix_does_not_exceed_threshold() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = crate::memory::Builder::default()
        .path(dir.path())
        .dimension(4)
        .tuning(crate::core::options::Tuning {
            brute_force_max_rows: 64,
            ..crate::core::options::Tuning::default()
        })
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let batch: Vec<crate::memory::Record> = (0..64)
        .map(|row| crate::memory::Record::new(vec![row as f32, 1.0, 0.0, 0.0]))
        .collect();
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");

    SCORE_CALLS.with(|calls| calls.set(0));
    let hits = ns
        .search()
        .vector(&[1.0, 0.0, 0.0, 0.0])
        .top_k(4)
        .ef(16)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 4);
    let calls = SCORE_CALLS.with(std::cell::Cell::get);
    assert_eq!(
        calls, 64,
        "行数等于阈值(未严格超过)时必须走暴力;若索引存在即走图会在此变红"
    );
}

/// FC-QUANT-INV-015:两阶段粗排候选数 `= min(top_k × oversample, 候选总数)`,
/// 且仍返回 top_k;倍率为 8、top_k=4 时候选恰为 32,不得放大成全量扫描。
#[test]
fn coarse_candidates_respect_rescore_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = crate::memory::Builder::default()
        .path(dir.path())
        .dimension(4)
        .quantization(crate::core::options::VectorFormat::I8Rescored)
        .tuning(crate::core::options::Tuning {
            brute_force_max_rows: 8,
            rescore_oversample: 8,
            quant_recall_floor: 0.0,
            ..crate::core::options::Tuning::default()
        })
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let batch: Vec<crate::memory::Record> = (0..128)
        .map(|row| crate::memory::Record::new(vec![row as f32, 1.0, (row % 7) as f32, 1.0]))
        .collect();
    ns.insert_batch(batch).expect("insert_batch");
    db.flush().expect("flush");

    COARSE_CANDIDATES.with(|calls| calls.set(0));
    let hits = ns
        .search()
        .vector(&[1.0, 0.0, 0.0, 0.0])
        .top_k(4)
        .ef(16)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 4);
    let coarse = COARSE_CANDIDATES.with(std::cell::Cell::get);
    assert_eq!(coarse, 32, "粗排候选应为 top_k × oversample = 32");
}

/// FC-QUANT-INV-015(重排口径):精排排序键按度量方向 + `RowId` 去平,
/// 与粗排插入次序无关;分数含 `NaN` 时仍给出确定性全序(不破坏 `sort_by`
/// 的严格弱序要求)。
#[test]
fn rescore_ordering_is_metric_aware_and_total() {
    let scored = |rowid: u64, score: f32| Scored {
        slot: SlotId::new(rowid as u32),
        rowid: RowId::new(rowid),
        score,
    };
    let mut dot = [
        scored(1, 0.5),
        scored(2, 2.0),
        scored(3, 2.0),
        scored(4, f32::NAN),
    ];
    dot.sort_by(|left, right| compare_scored(Metric::Dot, left, right));
    assert_eq!(
        dot.iter().map(|hit| hit.rowid.get()).collect::<Vec<_>>(),
        vec![2, 3, 1, 4],
        "Dot:高分在前,同分按 RowId 升序,NaN 排末"
    );
    let mut euclidean = [scored(1, 0.5), scored(2, 2.0)];
    euclidean.sort_by(|left, right| compare_scored(Metric::Euclidean, left, right));
    assert_eq!(euclidean[0].rowid.get(), 1, "Euclidean:低分(更近)在前");
}
