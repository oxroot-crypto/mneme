//! L1 检索 / 过滤 / 打分契约验收测试。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-MEM-CPLX-001..003/005(暴力扫描、过滤求值、并行归并、流式 iter)
//! * FC-INDEX-POST-003/004、FC-QUERY-ERR-002、FC-QUERY-POST-001
//! * FC-SCORE-INV-027、FC-SCORE-POST-001

use mneme::{Expr, Feedback, Metric, Mneme, Record, Scoring};
use proptest::prelude::*;

mod common;

use common::{inserted, mem, reference_dot};

/// FC-MEM-CPLX-001 / FC-MEM-INV-003(暴力检索 ≡ 参考实现)
#[test]
fn brute_force_matches_reference() {
    let ns = mem(4).namespace("n");
    let vectors = [
        vec![1.0, 0.0, 0.0, 0.0],
        vec![0.0, 1.0, 0.0, 0.0],
        vec![0.5, 0.5, 0.0, 0.0],
        vec![0.0, 0.0, 1.0, 0.0],
    ];
    let mut expected = Vec::new();
    for (index, vector) in vectors.iter().enumerate() {
        let id = inserted(
            ns.insert(Record::new(vector.clone()).key(format!("k{index}")))
                .expect("insert"),
        );
        expected.push((id.get(), vector.clone()));
    }
    let query = [0.9_f32, 0.1, 0.0, 0.0];
    let hits = ns
        .search()
        .vector(&query)
        .top_k(3)
        .execute()
        .expect("search");
    let got: Vec<u64> = hits.iter().map(|hit| hit.rowid.get()).collect();
    assert_eq!(got, reference_dot(&query, &expected, 3));
}

/// FC-QUERY-ERR-002 / FC-MEM-CPLX-002
#[test]
fn filter_uses_kleene_three_valued_logic() {
    let ns = mem(2).namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("k"))
        .expect("insert");
    let not_kind = Expr::Not(Box::new(Expr::field("kind").eq("x")));
    assert!(
        ns.search()
            .vector(&[1.0, 0.0])
            .filter(not_kind)
            .execute()
            .expect("search")
            .is_empty(),
        "缺失字段时 Not 不得命中"
    );
    let exists = Expr::Exists("kind".into());
    assert!(ns.count(Some(exists)).expect("count") == 0);
    ns.insert(
        Record::new(vec![0.0, 1.0])
            .key("j")
            .metadata(mneme::json!({"kind": "x"})),
    )
    .expect("insert");
    assert_eq!(
        ns.count(Some(Expr::Exists("kind".into()))).expect("count"),
        1
    );
}

/// FC-MEM-INV-003 / FC-INDEX-POST-003 / FC-MEM-CPLX-003
#[test]
fn search_order_is_total_and_stable() {
    let ns = mem(2).namespace("n");
    for index in 0..8 {
        ns.insert(Record::new(vec![1.0, 0.0]).key(format!("k{index}")))
            .expect("insert");
    }
    let first: Vec<u64> = ns
        .search()
        .vector(&[1.0, 0.0])
        .top_k(5)
        .execute()
        .expect("search")
        .iter()
        .map(|hit| hit.rowid.get())
        .collect();
    let second: Vec<u64> = ns
        .search()
        .vector(&[1.0, 0.0])
        .top_k(5)
        .execute()
        .expect("search")
        .iter()
        .map(|hit| hit.rowid.get())
        .collect();
    assert_eq!(first, second, "同快照内结果必须逐位一致");
    let mut sorted = first.clone();
    sorted.sort_unstable();
    assert_eq!(first, sorted, "同分按 RowId 升序");
}

/// FC-MEM-CPLX-005
#[test]
fn iter_streams_filtered_records() {
    let ns = mem(2).namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a").importance(0.9))
        .expect("insert");
    ns.insert(Record::new(vec![0.0, 1.0]).key("b").importance(0.1))
        .expect("insert");
    let collected: Vec<String> = ns
        .iter(Some(Expr::field("importance").gt(0.5_f32)))
        .expect("iter")
        .map(|rec| rec.expect("row").key().expect("key").to_string())
        .collect();
    assert_eq!(collected, vec!["a".to_string()]);
}

/// FC-MEM-PRE-003 / FC-GLOBAL-PRE-004(`top_k`/`ef` 超上限)
#[test]
fn search_limits_reject_top_k_and_ef() {
    let ns = mem(2).namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0])).expect("insert");
    assert!(matches!(
        ns.search().vector(&[1.0, 0.0]).top_k(4_097).execute(),
        Err(mneme::MnemeError::LimitExceeded { field: "top_k", .. })
    ));
    assert!(matches!(
        ns.search().vector(&[1.0, 0.0]).ef(4_097).execute(),
        Err(mneme::MnemeError::LimitExceeded { field: "ef", .. })
    ));
    // 边界值恰好等于上限时接受
    assert!(
        ns.search()
            .vector(&[1.0, 0.0])
            .top_k(4_096)
            .execute()
            .is_ok(),
        "Bound 必须接受"
    );
}

/// FC-MEM-PRE-004 / FC-GLOBAL-PRE-001(检索维度校验)
#[test]
fn search_rejects_dimension_mismatch() {
    let ns = mem(3).namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0, 0.0])).expect("insert");
    assert!(matches!(
        ns.search().vector(&[1.0, 0.0]).execute(),
        Err(mneme::MnemeError::DimensionMismatch {
            expected: 3,
            got: 2
        })
    ));
}

/// FC-QUERY-POST-001(谓词类型规则)
#[test]
fn predicate_type_rules() {
    let ns = mem(2).namespace("n");
    ns.insert(
        Record::new(vec![1.0, 0.0])
            .key("k")
            .metadata(mneme::json!({"n": 3, "s": "hello"})),
    )
    .expect("insert");
    // 数值比较 Int/Num 互通
    assert_eq!(
        ns.count(Some(Expr::field("n").gt(2.5_f64))).expect("count"),
        1
    );
    // Ts 仅与 Ts 比较:valid_from 为 Ts,用 Num 比较 → Unknown(不命中)
    assert_eq!(
        ns.count(Some(Expr::field("valid_from").eq(0.0_f64)))
            .expect("count"),
        0
    );
    // 字符串算子要求字符串:数值字段 → Unknown
    assert_eq!(
        ns.count(Some(Expr::StartsWith("n".into(), "3".into())))
            .expect("count"),
        0
    );
    // 字符串字段正常命中
    assert_eq!(
        ns.count(Some(Expr::StartsWith("s".into(), "he".into())))
            .expect("count"),
        1
    );
}

/// FC-SCORE-INV-027
#[test]
fn feedback_is_idempotent_per_query() {
    let ns = mem(2).namespace("n");
    let id = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).key("k"))
            .expect("insert"),
    );
    let query_id = mneme::QueryId(7);
    assert!(ns.feedback(id, Feedback::Used, query_id).expect("feedback"));
    assert!(
        !ns.feedback(id, Feedback::Used, query_id).expect("feedback"),
        "同一 (rowid, query_id) 至多计一次"
    );
    assert!(
        ns.feedback(id, Feedback::Used, mneme::QueryId(8))
            .expect("feedback")
    );
}

/// FC-SCORE-POST-001
#[test]
fn default_scoring_matches_similarity_order() {
    let ns = mem(3).namespace("n");
    for (index, vector) in [
        vec![1.0, 0.0, 0.0],
        vec![0.8, 0.2, 0.0],
        vec![0.0, 1.0, 0.0],
    ]
    .iter()
    .enumerate()
    {
        ns.insert(Record::new(vector.clone()).key(format!("k{index}")))
            .expect("insert");
    }
    let plain: Vec<u64> = ns
        .search()
        .vector(&[1.0, 0.0, 0.0])
        .top_k(3)
        .execute()
        .expect("search")
        .iter()
        .map(|hit| hit.rowid.get())
        .collect();
    let scored: Vec<u64> = ns
        .search()
        .vector(&[1.0, 0.0, 0.0])
        .score(Scoring::default())
        .top_k(3)
        .execute()
        .expect("search")
        .iter()
        .map(|hit| hit.rowid.get())
        .collect();
    assert_eq!(plain, scored);
}

proptest! {
    /// FC-MEM-CPLX-001(随机向量下暴力检索 ≡ 参考实现)
    #[test]
    fn brute_force_matches_reference_prop(
        records in prop::collection::vec(prop::collection::vec(-1.0f32..1.0, 4), 1..24),
        query in prop::collection::vec(-1.0f32..1.0, 4),
        k in 1usize..8,
    ) {
        let db = Mneme::builder().dimension(4).metric(Metric::Dot).build().expect("build");
        let ns = db.namespace("n");
        let mut expected = Vec::new();
        for vector in &records {
            let id = inserted(ns.insert(Record::new(vector.clone())).expect("insert"));
            expected.push((id.get(), vector.clone()));
        }
        let hits = ns.search().vector(&query).top_k(k).execute().expect("search");
        let got: Vec<u64> = hits.iter().map(|hit| hit.rowid.get()).collect();
        prop_assert_eq!(got, reference_dot(&query, &expected, k));
    }

    /// FC-MEM-POST-005 / FC-MEM-CPLX-002(count 与过滤语义一致)
    #[test]
    fn filter_matches_bruteforce_prop(
        importances in prop::collection::vec(0.0f32..1.0, 1..32),
        threshold in 0.0f32..1.0,
    ) {
        let ns = mem(2).namespace("n");
        let mut expected = 0_u64;
        for (index, importance) in importances.iter().enumerate() {
            ns.insert(
                Record::new(vec![1.0, 0.0])
                    .key(format!("k{index}"))
                    .importance(*importance),
            )
            .expect("insert");
            if *importance > threshold {
                expected += 1;
            }
        }
        let count = ns
            .count(Some(Expr::field("importance").gt(threshold)))
            .expect("count");
        prop_assert_eq!(count, expected);
    }
}
