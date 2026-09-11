//! L1 检索 / 过滤 / 打分契约验收测试。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-MEM-CPLX-001..003/005(暴力扫描、过滤求值、并行归并、iter 过滤/排序)
//! * FC-MEM-PRE-003/004、FC-MEM-POST-005、FC-MEM-INV-003
//! * FC-INDEX-INV-005、FC-INDEX-POST-003、FC-QUERY-ERR-002、FC-QUERY-POST-001
//! * FC-SCORE-INV-027、FC-SCORE-POST-001、FC-SCORE-POST-002、FC-GLOBAL-PRE-001/004

use std::sync::Arc;

use mneme::{Diversity, Expr, Feedback, Metric, Mneme, Record, Scoring};
use proptest::prelude::*;

mod common;

use common::{FakeClock, inserted, mem, reference_dot};

/// FC-MEM-CPLX-001(暴力检索 ≡ 参考实现)
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

/// FC-MEM-CPLX-005(过滤 + RowId 升序 + 墓碑常规路径不可见)
#[test]
fn iter_filters_and_sorts_by_rowid() {
    let ns = mem(2).namespace("n");
    // 先写 "z" 再写 "a":RowId 升序与字典序相反,可观测"按 RowId 而非 key 排序"。
    ns.insert(Record::new(vec![1.0, 0.0]).key("z").importance(0.9))
        .expect("insert");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a").importance(0.9))
        .expect("insert");
    ns.insert(Record::new(vec![0.0, 1.0]).key("m").importance(0.1))
        .expect("insert");
    let collected: Vec<String> = ns
        .iter(Some(Expr::field("importance").gt(0.5_f32)))
        .expect("iter")
        .map(|rec| rec.expect("row").key().expect("key").to_string())
        .collect();
    assert_eq!(
        collected,
        vec!["z".to_string(), "a".to_string()],
        "预过滤命中且按 RowId 升序"
    );
    // 墓碑在常规 iter 不可见,iter_with(_, true) 审计入口可见(I9)。
    ns.delete("z").expect("delete");
    assert_eq!(ns.iter(None).expect("iter").count(), 2);
    assert_eq!(ns.iter_with(None, true).expect("iter_with").count(), 3);
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

/// FC-SCORE-INV-027(同键幂等;不可见记录——不存在/已墓碑/已过期——返回 false
/// 且不占用幂等键,该键随后仍可用于可见记录)
#[test]
fn feedback_is_idempotent_per_query() {
    let clock = FakeClock::default();
    clock.set(1_000);
    let db = Mneme::builder()
        .dimension(2)
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("n");
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
    // 不存在:返回 false 且不占用幂等键(该键随后对可见记录仍生效)。
    assert!(
        !ns.feedback(mneme::RowId::new(999), Feedback::Used, mneme::QueryId(9))
            .expect("feedback")
    );
    assert!(
        ns.feedback(id, Feedback::Used, mneme::QueryId(9))
            .expect("feedback"),
        "不可见记录的 feedback 不得占用幂等键"
    );
    // 已墓碑:delete 后不可见;该键仍可用于后续写入的新记录。
    ns.delete("k").expect("delete");
    assert!(
        !ns.feedback(id, Feedback::Used, mneme::QueryId(10))
            .expect("feedback")
    );
    let new_id = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).key("k2"))
            .expect("insert"),
    );
    assert!(
        ns.feedback(new_id, Feedback::Used, mneme::QueryId(10))
            .expect("feedback"),
        "墓碑处未占用的幂等键可用于新记录"
    );
    // 已过期(TTL 到期):不可见,绝不强化。
    let ttl_id = inserted(
        ns.insert(
            Record::new(vec![1.0, 0.0])
                .key("k3")
                .ttl(std::time::Duration::from_millis(500)),
        )
        .expect("insert"),
    );
    clock.set(1_501);
    assert!(
        !ns.feedback(ttl_id, Feedback::Used, mneme::QueryId(11))
            .expect("feedback"),
        "已过期记录对 feedback 不可见"
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

/// FC-SCORE-POST-002:归一化相似度低于 `floor` 时综合分清零;等于 `floor` 为保留边界。
#[test]
fn scoring_floor_zeroes_below_threshold() {
    let ns = mem(2).namespace("n");
    // 查询 [1,0];三条候选的归一化相似度分别为 1.0 / 1/√2 / 0.0(min = 0,span = 1)。
    for (index, vector) in [vec![1.0, 0.0], vec![0.5, 0.5], vec![0.0, 1.0]]
        .iter()
        .enumerate()
    {
        ns.insert(Record::new(vector.clone()).key(format!("k{index}")))
            .expect("insert");
    }
    let scores = |floor: f32| -> Vec<f32> {
        ns.search()
            .vector(&[1.0, 0.0])
            .score(Scoring {
                floor,
                ..Scoring::default()
            })
            .top_k(3)
            .execute()
            .expect("search")
            .iter()
            .map(|hit| hit.score)
            .collect()
    };
    let almost = |got: &[f32], want: &[f32]| {
        got.len() == want.len() && got.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-6)
    };
    let boundary = std::f32::consts::FRAC_1_SQRT_2;
    // floor = 1/√2:ŝ = 1/√2(等于边界)保留,ŝ = 0.0 清零。
    assert!(almost(&scores(boundary), &[1.0, boundary, 0.0]));
    // floor = 0.8(高于 1/√2):ŝ = 1/√2 被清零,只剩首条非零(证伪"恒不清零")。
    assert!(almost(&scores(0.8), &[1.0, 0.0, 0.0]));
    // floor = 0:任何非负相似度都不清零,与不做保底全等。
    assert!(almost(&scores(0.0), &[1.0, boundary, 0.0]));
    // floor = 0 必须真正"不清零":开 importance 权重使尾部候选总分为正,
    // 若实现退化为 `<=` 清零,该断言变红(消除上面恒等比较的空转)。
    let floor_zero = ns
        .search()
        .vector(&[1.0, 0.0])
        .score(Scoring {
            floor: 0.0,
            w_importance: 1.0,
            ..Scoring::default()
        })
        .top_k(3)
        .execute()
        .expect("search");
    let tail_score = floor_zero.last().expect("3 hits").score;
    assert!(
        tail_score > 0.0,
        "floor=0 时相似度 0 的候选不得被清零(实现必须用严格小于)"
    );
}

/// FC-MEM-PRE-003(综合打分各因子钳制到 [0,1]:访问频次因子超基准后恒为 1.0;
/// MMR lambda 越界钳制,5.0 ≡ 1.0、-2.0 ≡ 0.0)
#[test]
fn scoring_composite_factors_clamped() {
    let ns = mem(2).namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("none"))
        .expect("insert");
    ns.insert(Record::new(vec![0.0, 1.0]).key("bound"))
        .expect("insert");
    ns.insert(Record::new(vec![1.0, 1.0]).key("far"))
        .expect("insert");
    // 访问 101 次与 250 次均超过 c_norm=100 → 因子钳制为 1.0,得分相同;
    // 未访问记录因子为 0(三点:0 / Bound / 远超上界)。
    for _ in 0..101 {
        ns.touch("bound", None).expect("touch");
    }
    for _ in 0..250 {
        ns.touch("far", None).expect("touch");
    }
    let scoring = Scoring {
        w_sim: 0.0,
        w_access: 1.0,
        ..Scoring::default()
    };
    let hits = ns
        .search()
        .vector(&[1.0, 0.0])
        .score(scoring)
        .top_k(3)
        .execute()
        .expect("search");
    let scores: Vec<f32> = hits.iter().map(|hit| hit.score).collect();
    assert_eq!(scores[0], 1.0, "Bound+1(101 次)钳制为 1.0");
    assert_eq!(scores[1], 1.0, "远超上限(250 次)同样钳制为 1.0");
    assert_eq!(scores[2], 0.0, "未访问记录访问因子为 0");

    // MMR lambda 越界钳制:超界值与对应边界值的命中序列全等(同快照排序全等)。
    for vector in [
        vec![1.0, 0.0],
        vec![1.0, 0.0],
        vec![0.0, 1.0],
        vec![0.0, 1.0],
    ] {
        ns.insert(Record::new(vector)).expect("insert");
    }
    let order = |lambda: f32| -> Vec<u64> {
        ns.search()
            .vector(&[1.0, 0.0])
            .top_k(4)
            .diversify(Diversity::Mmr { lambda })
            .execute()
            .expect("search")
            .iter()
            .map(|hit| hit.rowid.get())
            .collect()
    };
    assert_eq!(order(5.0), order(1.0), "lambda 越上界钳制为 1.0");
    assert_eq!(order(-2.0), order(0.0), "负 lambda 钳制为 0.0");

    // 非有限 lambda:`clamp` 对 NaN 失效,会静默退化为固定取首项,入口必须拒绝
    // (FC-MEM-PRE-003 / FC-GLOBAL-PRE-004,拒绝静默失败)。
    for lambda in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(matches!(
            ns.search()
                .vector(&[1.0, 0.0])
                .diversify(Diversity::Mmr { lambda })
                .execute(),
            Err(mneme::MnemeError::Config { .. })
        ));
    }
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

    /// FC-MEM-POST-005(count 与过滤语义一致)
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
