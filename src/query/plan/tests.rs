use super::*;

use crate::core::types::NsId;
use crate::memory::pred::{self, Expr, Val};
use crate::memory::search::{self, CandidateQuery};
use crate::memory::table::ReaderView;
use crate::memory::{Mneme, Record};

use super::compile::{PLAN_CACHE_HITS, PLAN_CACHE_STORES};
use super::filter::{ROW_EVALS, TTL_CHECKS};

/// 建库并返回 `(db, ns_id)`;前 1024 条 `importance=0.1`,随后 6 条 `0.9`,
/// 使块 0 可被 `importance > 0.5` 整体剪除。
fn setup() -> (Mneme, NsId) {
    let db = Mneme::in_memory(2).expect("in_memory");
    let ns = db.namespace("n");
    for index in 0..1030 {
        let importance = if index < 1024 { 0.1_f32 } else { 0.9 };
        ns.insert(
            Record::new(vec![1.0, 0.0])
                .key(format!("k{index}"))
                .importance(importance)
                .metadata(crate::core::meta::json!({
                    "rank": index,
                    "kind": if index % 3 == 0 { "a" } else { "b" },
                })),
        )
        .expect("insert");
    }
    let view = db.table.view();
    let ns_id = view
        .ns_registry
        .iter()
        .find_map(|(id, path)| (**path == *"n").then_some(*id))
        .expect("命名空间已注册");
    (db, ns_id)
}

/// 断言计划候选与逐行求值全等,并返回计划候选数。
fn assert_matches_bruteforce(view: &ReaderView, ns_id: NsId, expr: &Expr) -> usize {
    let plan = compile(view, ns_id, Some(expr), 0);
    let brute = search::collect_candidates(&CandidateQuery {
        view,
        ns_id,
        filter: Some(expr),
        now_ms: 0,
    });
    assert_eq!(
        plan.candidates(),
        brute.as_slice(),
        "下推不得改变候选: {expr}"
    );
    plan.candidates().len()
}

/// FC-QUERY-POST-005 / FC-QUERY-CPLX-002(块级剪枝与逐行求值全等)
#[test]
fn plan_candidates_match_bruteforce() {
    let (db, ns_id) = setup();
    let view = db.table.view();
    for expr in [
        Expr::field("importance").gt(0.5_f32),
        Expr::field("importance").ge(0.9_f32),
        Expr::field("importance").le(0.1_f32),
        Expr::field("rank").lt(20_i64),
        Expr::field("rank").ne(5_i64),
        Expr::field("kind").eq("a"),
        Expr::Exists("rank".into()),
        Expr::IsNull("rank".into()),
        Expr::In(
            "rank".into(),
            vec![Val::Int(1), Val::Int(2), Val::Int(1025)].into_boxed_slice(),
        ),
        Expr::field("importance").gt(0.5_f32) & Expr::field("rank").lt(1028_i64),
        Expr::Not(Box::new(Expr::field("importance").gt(0.5_f32))),
        Expr::field("importance").eq(0.1_f32) | Expr::Never,
        Expr::Always,
        Expr::Never,
    ] {
        assert_matches_bruteforce(&view, ns_id, &expr);
    }
}

/// FC-QUERY-POST-005(块级剪枝真实生效:块 0 被整体剪除)
#[test]
fn block_pruning_skips_impossible_blocks() {
    let (db, ns_id) = setup();
    let view = db.table.view();
    let expr = Expr::field("importance").gt(0.5_f32);
    let count = assert_matches_bruteforce(&view, ns_id, &expr);
    assert_eq!(count, 6, "块 0(max=0.1)应被剪除,只剩后 6 条");
}

/// FC-QUERY-POST-005(`key` 等值经 bloom 预筛;否定即零候选)
#[test]
fn key_bloom_rejects_absent_key() {
    let (db, ns_id) = setup();
    let view = db.table.view();
    let count = assert_matches_bruteforce(&view, ns_id, &Expr::field("key").eq("no-such-key"));
    assert_eq!(count, 0, "bloom 否定必须直接得到空候选");
    assert_matches_bruteforce(&view, ns_id, &Expr::field("key").eq("k1028"));
}

/// FC-QUERY-POST-005(bloom 否定必须避免行级求值,证伪"预筛未被走到")
#[test]
fn key_bloom_skips_row_evaluation() {
    let (db, ns_id) = setup();
    let view = db.table.view();
    ROW_EVALS.with(|evals| evals.set(0));
    let count = assert_matches_bruteforce(&view, ns_id, &Expr::field("key").eq("no-such-key"));
    assert_eq!(count, 0);
    assert_eq!(
        ROW_EVALS.with(std::cell::Cell::get),
        0,
        "bloom 否定应整块剪除候选,不触发任何行级求值"
    );
    // 对照:命中存在的 key 时仍有行级求值,防止探针自身失效造成假绿。
    ROW_EVALS.with(|evals| evals.set(0));
    assert_matches_bruteforce(&view, ns_id, &Expr::field("key").eq("k1028"));
    assert!(ROW_EVALS.with(std::cell::Cell::get) > 0);
}

/// FC-QUERY-POST-005(选择性 = 候选 / 活行)
#[test]
fn selectivity_reflects_candidate_ratio() {
    let (db, ns_id) = setup();
    let view = db.table.view();
    let plan = compile(&view, ns_id, None, 0);
    assert_eq!(plan.candidates.len(), 1030);
    assert!((plan.selectivity - 1.0).abs() < 1e-6);
    let filtered = compile(
        &view,
        ns_id,
        Some(&Expr::field("importance").gt(0.5_f32)),
        0,
    );
    assert!((filtered.selectivity - 6.0 / 1030.0).abs() < 1e-6);
}

/// FC-QUERY-POST-008:无过滤且无 TTL 行时同视图重复编译命中缓存(候选与
/// 逐行求值全等);含 TTL 行的命名空间不写不命中(过期实时反映);写事务
/// 发布的新视图不复用旧视图缓存。
#[test]
fn plan_cache_is_transparent_and_ttl_aware() {
    use std::time::Duration;

    let (db, ns_id) = setup();
    let view = db.table.view();
    PLAN_CACHE_HITS.with(|count| count.set(0));
    PLAN_CACHE_STORES.with(|count| count.set(0));
    let first = compile(&view, ns_id, None, 0);
    let second = compile(&view, ns_id, None, 0);
    assert_eq!(first.candidates(), second.candidates());
    assert_eq!(first.selectivity, second.selectivity);
    assert_eq!(
        PLAN_CACHE_STORES.with(std::cell::Cell::get),
        1,
        "首次编译写一次缓存"
    );
    assert_eq!(
        PLAN_CACHE_HITS.with(std::cell::Cell::get),
        1,
        "第二次编译命中缓存"
    );
    let brute = search::collect_candidates(&CandidateQuery {
        view: &view,
        ns_id,
        filter: None,
        now_ms: 0,
    });
    assert_eq!(
        first.candidates(),
        brute.as_slice(),
        "缓存候选必须与逐行求值全等"
    );

    // 含 TTL 行:不写缓存、不命中;过期行随时间实时消失(绝不因缓存复活)。
    let ttl_db = Mneme::in_memory(2).expect("in_memory");
    let ttl_ns = ttl_db.namespace("t");
    ttl_ns
        .insert(
            Record::new(vec![1.0, 0.0])
                .key("a")
                .ttl(Duration::from_millis(30_000)),
        )
        .expect("insert ttl");
    ttl_ns
        .insert(Record::new(vec![0.0, 1.0]).key("b"))
        .expect("insert plain");
    let ttl_view = ttl_db.table.view();
    let ttl_ns_id = ttl_view
        .ns_registry
        .iter()
        .find_map(|(id, path)| (**path == *"t").then_some(*id))
        .expect("命名空间已注册");
    PLAN_CACHE_HITS.with(|count| count.set(0));
    PLAN_CACHE_STORES.with(|count| count.set(0));
    assert_eq!(compile(&ttl_view, ttl_ns_id, None, 0).candidates().len(), 2);
    assert_eq!(compile(&ttl_view, ttl_ns_id, None, 0).candidates().len(), 2);
    assert_eq!(
        PLAN_CACHE_STORES.with(std::cell::Cell::get),
        0,
        "含 TTL 行不得写缓存"
    );
    assert_eq!(
        PLAN_CACHE_HITS.with(std::cell::Cell::get),
        0,
        "含 TTL 行不得命中缓存"
    );
    assert_eq!(
        compile(&ttl_view, ttl_ns_id, None, i64::MAX)
            .candidates()
            .len(),
        1,
        "过期行必须随时间实时消失"
    );

    // 新视图不复用旧缓存:新写入行必须立刻可见。
    ttl_ns
        .insert(Record::new(vec![1.0, 1.0]).key("c"))
        .expect("insert c");
    let new_view = ttl_db.table.view();
    assert_eq!(compile(&new_view, ttl_ns_id, None, 0).candidates().len(), 3);
}

/// FC-QUERY-POST-009:乱序合取链重排后候选与逐行暴力全等;高选择分支的
/// 短路必须真实减少昂贵谓词(`Contains`)的求值次数。
#[test]
fn reordered_conjunctions_match_bruteforce_and_short_circuit() {
    let db = Mneme::in_memory(2).expect("in_memory");
    let ns = db.namespace("n");
    let batch: Vec<Record> = (0..1024)
        .map(|index| {
            Record::new(vec![1.0, 0.0])
                .key(format!("k{index}"))
                .text("plain")
        })
        .collect();
    ns.insert_batch(batch).expect("batch");
    ns.insert(Record::new(vec![0.0, 1.0]).key("target").text("needle"))
        .expect("target");
    let view = db.table.view();
    let ns_id = view
        .ns_registry
        .iter()
        .find_map(|(id, path)| (**path == *"n").then_some(*id))
        .expect("命名空间已注册");

    // 昂贵且低选择的 `contains` 写在合取链最前,高选择 `key` 等值在后(乱序输入)。
    let expr =
        Expr::Contains("key".into(), Val::Str("get".into())) & Expr::field("key").eq("target");
    let brute = search::collect_candidates(&CandidateQuery {
        view: &view,
        ns_id,
        filter: Some(&expr),
        now_ms: 0,
    });
    assert_eq!(brute.len(), 1, "仅 target 行命中");
    pred::reset_contains_evals();
    let plan = compile(&view, ns_id, Some(&expr), 0);
    assert_eq!(plan.candidates(), brute.as_slice(), "重排不得改变候选集合");
    assert_eq!(
        pred::contains_eval_count(),
        1,
        "重排后 `key` 等值先行,昂贵 `contains` 只对命中行求值一次"
    );
}

/// FC-QUERY-POST-005(保留字段被同名 metadata 影子化:不得据 metadata 统计剪块)
#[test]
fn reserved_metadata_shadowing_never_prunes() {
    let db = Mneme::in_memory(2).expect("in_memory");
    let ns = db.namespace("n");
    for index in 0..1024 {
        ns.insert(Record::new(vec![1.0, 0.0]).key(format!("k{index}")))
            .expect("insert");
    }
    // 第二条块(槽位 1024)的 metadata 与保留字段同名:不得据此剪掉块 0。
    ns.insert(
        Record::new(vec![0.0, 1.0])
            .key("shadow")
            .metadata(crate::core::meta::json!({
                "key": 5,
                "rowid": 9_000,
                "access_count": 7,
                "last_access": 3,
            })),
    )
    .expect("insert");
    let view = db.table.view();
    let ns_id = view
        .ns_registry
        .iter()
        .find_map(|(id, path)| (**path == *"n").then_some(*id))
        .expect("命名空间已注册");
    for expr in [
        Expr::Exists("key".into()),
        Expr::Exists("rowid".into()),
        Expr::field("key").ne("nope"),
        Expr::field("rowid").gt(0_i64),
        Expr::field("access_count").ge(0_i64),
        Expr::field("last_access").ge(0_i64),
        Expr::In("key".into(), vec![Val::Str("k1".into())].into_boxed_slice()),
    ] {
        assert_matches_bruteforce(&view, ns_id, &expr);
    }
    let plan = compile(&view, ns_id, Some(&Expr::Exists("key".into())), 0);
    assert_eq!(plan.candidates.len(), 1025, "所有保留 key 都必须保留");
}

/// FC-QUERY-POST-005(保留名对象下的子路径 `key.x` 按 `meta::get_path` 照常观察)
#[test]
fn dotted_reserved_subpath_never_prunes() {
    let db = Mneme::in_memory(2).expect("in_memory");
    let ns = db.namespace("n");
    for index in 0..1024 {
        let record = Record::new(vec![1.0, 0.0]).key(format!("k{index}"));
        // 块 0 内嵌 `{"key": {"x": 5}}`:行级 `key.x == 5` 可命中。
        let record = if index == 0 {
            record.metadata(crate::core::meta::json!({"key": {"x": 5}}))
        } else {
            record
        };
        ns.insert(record).expect("insert");
    }
    // 块 1 用字面点分键把 `key.x` 注册进 zone map(数字/字符串两种形态)。
    ns.insert(
        Record::new(vec![0.0, 1.0])
            .key("dotted-num")
            .metadata(crate::core::meta::json!({"key.x": 9})),
    )
    .expect("insert");
    ns.insert(
        Record::new(vec![0.0, 1.0])
            .key("dotted-str")
            .metadata(crate::core::meta::json!({"key.x": "s"})),
    )
    .expect("insert");
    let view = db.table.view();
    let ns_id = view
        .ns_registry
        .iter()
        .find_map(|(id, path)| (**path == *"n").then_some(*id))
        .expect("命名空间已注册");
    for expr in [
        Expr::field("key.x").eq(5_i64),
        Expr::field("key.x").eq(9_i64),
        Expr::field("key.x").lt(8_i64),
        Expr::Exists("key.x".into()),
    ] {
        assert_matches_bruteforce(&view, ns_id, &expr);
    }
    let plan = compile(&view, ns_id, Some(&Expr::field("key.x").eq(5_i64)), 0);
    assert_eq!(plan.candidates.len(), 1, "嵌套子路径行必须保留");
}

/// FC-LIFE-CPLX-001(TTL 块级剪枝:整块无过期风险时零逐行 TTL 判定)
#[test]
fn ttl_unexpired_block_skips_per_row_checks() {
    let db = Mneme::in_memory(2).expect("in_memory");
    let ns = db.namespace("n");
    // 块 0(1024 行):无 TTL —— 整块可证明未过期。
    let plain: Vec<Record> = (0..1024)
        .map(|index| Record::new(vec![1.0, 0.0]).key(format!("p{index}")))
        .collect();
    ns.insert_batch(plain).expect("plain batch");
    // 块 1(1024 行):带 1ms TTL,配合 `now = i64::MAX` 全部逻辑过期。
    let expiring: Vec<Record> = (0..1024)
        .map(|index| {
            Record::new(vec![0.0, 1.0])
                .key(format!("t{index}"))
                .ttl(std::time::Duration::from_millis(1))
        })
        .collect();
    ns.insert_batch(expiring).expect("ttl batch");
    let view = db.table.view();
    let ns_id = view
        .ns_registry
        .iter()
        .find_map(|(id, path)| (**path == *"n").then_some(*id))
        .expect("命名空间已注册");

    TTL_CHECKS.with(|checks| checks.set(0));
    let plan = compile(&view, ns_id, None, i64::MAX);
    assert_eq!(plan.candidates.len(), 1024, "过期 TTL 行不入候选");
    assert_eq!(
        TTL_CHECKS.with(std::cell::Cell::get),
        1024,
        "仅第二块(存在 TTL 值)需要逐行判定,块 0 必须零判定"
    );
}
