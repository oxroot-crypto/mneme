//! L1 检索 / 过滤 / 打分契约验收测试。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-MEM-CPLX-001..003/005(暴力扫描、过滤求值、并行归并、iter 过滤/排序)
//! * FC-MEM-PRE-003/004、FC-MEM-POST-005、FC-MEM-INV-003
//! * FC-INDEX-INV-005、FC-INDEX-POST-003、FC-QUERY-ERR-002、FC-QUERY-POST-001
//! * FC-SCORE-INV-027、FC-SCORE-POST-001、FC-SCORE-POST-002、FC-SCORE-POST-004/005/006/007、FC-GLOBAL-PRE-001/004
//! * FC-MEM-ERR-002(`Scoring::bias_routing` 落地后不再返回 `Unsupported`)
//! * FC-SCORE-CPLX-001..003
//!
//! 不变量锚定:I5(混合检索等价性)、I27(反馈幂等)

use std::sync::Arc;

use mneme::{
    Diversity, Expr, Feedback, Metric, Mneme, Record, RelationExpand, ResultDedup, Scoring,
};
use proptest::prelude::*;

mod common;

use common::{FakeClock, inserted, mem, reference_dot};

/// FC-MEM-CPLX-001(I5:暴力检索 ≡ 参考实现)
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

/// FC-SCORE-POST-004 / FC-SCORE-CPLX-002(联想扩展只沿同命名空间边推进;
/// 跨命名空间边即使经 `relate` 记录也视为不存在)
#[test]
fn expansion_does_not_cross_namespaces() {
    use mneme::RelationKind;

    let db = mem(4);
    let a = db.namespace("a");
    let b = db.namespace("b");
    let seed = inserted(
        a.insert(Record::new(vec![1.0, 0.0, 0.0, 0.0]).key("seed"))
            .expect("insert"),
    );
    let same_ns = inserted(
        a.insert(Record::new(vec![0.0, 0.0, 1.0, 0.0]).key("same"))
            .expect("insert"),
    );
    let other_ns = inserted(
        b.insert(Record::new(vec![0.0, 1.0, 0.0, 0.0]).key("other"))
            .expect("insert"),
    );
    a.relate(seed, same_ns, RelationKind::RELATED, 1.0)
        .expect("relate same ns");
    a.relate(seed, other_ns, RelationKind::RELATED, 1.0)
        .expect("relate cross ns");

    let hits = a
        .search()
        .vector(&[1.0, 0.0, 0.0, 0.0])
        .top_k(8)
        .expand(RelationExpand::default())
        .execute()
        .expect("search");
    let rowids: Vec<u64> = hits.iter().map(|hit| hit.rowid.get()).collect();
    assert!(rowids.contains(&seed.get()), "种子本身应命中");
    assert!(rowids.contains(&same_ns.get()), "同命名空间边应扩展命中");
    assert!(
        !rowids.contains(&other_ns.get()),
        "跨命名空间边必须视为不存在(FC-SCORE-POST-004)"
    );
}

/// FC-SCORE-POST-005(结果级去重三模式与阈值校验)
#[test]
fn result_dedup_modes_and_threshold_validation() {
    let db = mem(4);
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0, 0.0, 0.0]).key("a"))
        .expect("insert");
    ns.insert(Record::new(vec![1.0, 0.0, 0.0, 0.0]).key("b"))
        .expect("insert");
    let query = [1.0, 0.0, 0.0, 0.0];

    // Off(默认):两条同向记录都返回。
    let hits = ns
        .search()
        .vector(&query)
        .top_k(10)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 2);

    // ById:RowId 唯一(单通道下结果集不变)。
    let hits = ns
        .search()
        .vector(&query)
        .top_k(10)
        .dedup(ResultDedup::ById)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 2);
    assert_ne!(hits[0].rowid, hits[1].rowid);

    // Near 0.99:同向重复(余弦 1.0)只保留最高分一条。
    let hits = ns
        .search()
        .vector(&query)
        .top_k(10)
        .dedup(ResultDedup::Near { threshold: 0.99 })
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 1);

    // 边界 0.0/1.0 合法;NaN 与越界值在入口拒绝(绝不静默空转)。
    for threshold in [0.0_f32, 1.0] {
        ns.search()
            .vector(&query)
            .dedup(ResultDedup::Near { threshold })
            .execute()
            .expect("边界阈值合法");
    }
    for threshold in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        assert!(matches!(
            ns.search()
                .vector(&query)
                .dedup(ResultDedup::Near { threshold })
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

/// FC-SCORE-POST-006:扩展候选与既有通道候选按 `RowId` 合并,分数取
/// `max(自身分, boost)`(不重复出现);`via` 只在扩展确有贡献时记录。
#[test]
fn expansion_max_merges_with_existing_candidates() {
    use std::collections::HashSet;

    let ns = mem(2).namespace("n");
    let a = inserted(ns.insert(Record::new(vec![0.9, 0.1]).key("a")).expect("a"));
    let b = inserted(ns.insert(Record::new(vec![1.0, 0.0]).key("b")).expect("b"));
    let d = inserted(ns.insert(Record::new(vec![0.0, 1.0]).key("d")).expect("d"));
    ns.relate(a, b, mneme::RelationKind::RELATED, 0.3)
        .expect("a→b");
    ns.relate(a, d, mneme::RelationKind::RELATED, 0.8)
        .expect("a→d");
    let hits = ns
        .search()
        .vector(&[1.0, 0.0])
        .top_k(3)
        .ef(16)
        .expand(RelationExpand {
            hops: 1,
            kinds: vec![mneme::RelationKind::RELATED],
            decay: 0.5,
            max_nodes: 64,
        })
        .execute()
        .expect("search");

    let unique: HashSet<_> = hits.iter().map(|hit| hit.rowid).collect();
    assert_eq!(unique.len(), hits.len(), "同一 RowId 不得重复出现");
    let hit_b = hits
        .iter()
        .find(|hit| hit.key.as_ref().map(|key| key.as_str()) == Some("b"))
        .expect("b 在向量候选中");
    assert!(
        (hit_b.score - 1.0).abs() < 1e-6,
        "boost 低于自身相似度时必须保留自身分,实际 {}",
        hit_b.score
    );
    assert!(hit_b.via.is_none(), "扩展未提升分数时不得把 via 记成来源");
    let hit_d = hits
        .iter()
        .find(|hit| hit.key.as_ref().map(|key| key.as_str()) == Some("d"))
        .expect("d 由扩展进入");
    assert!(
        hit_d.score > 0.0,
        "boost 高于自身相似度时必须提升分数,实际 {}",
        hit_d.score
    );
    assert!(hit_d.via.is_some(), "扩展引入/提升的候选必须记录 via");
}

/// FC-SCORE-POST-007:偏置路由只改 HNSW 前沿出堆顺序;`ef` 收敛时返回的
/// 候选与分数与关闭时一致,且 `bias_routing = true` 不再返回 `Unsupported`。
#[test]
fn bias_routing_only_changes_visit_order() {
    use mneme::{Builder, Tuning};

    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .dimension(4)
        .path(dir.path())
        .tuning(Tuning {
            brute_force_max_rows: 1,
            ..Tuning::default()
        })
        .build()
        .expect("build");
    let ns = db.namespace("n");
    for row in 0..256_u32 {
        let vector: Vec<f32> = (0..4)
            .map(|col| ((row * 7 + col * 13) % 101) as f32 / 101.0)
            .collect();
        ns.insert(Record::new(vector).importance((row % 10) as f32 / 9.0))
            .expect("insert");
    }
    db.flush().expect("flush");
    let query = [0.5_f32, 0.5, 0.5, 0.5];
    let run = |bias: bool| {
        ns.search()
            .vector(&query)
            .top_k(10)
            .ef(2048)
            .score(Scoring {
                bias_routing: bias,
                w_importance: 0.2,
                ..Scoring::default()
            })
            .execute()
            .expect("search")
    };
    let plain = run(false);
    let biased = run(true);
    let plain_ids: Vec<_> = plain.iter().map(|hit| hit.rowid).collect();
    let biased_ids: Vec<_> = biased.iter().map(|hit| hit.rowid).collect();
    assert_eq!(plain_ids, biased_ids, "ef 收敛后偏置不得改变候选集或次序");
    for (left, right) in plain.iter().zip(&biased) {
        assert!(
            (left.score - right.score).abs() < 1e-6,
            "偏置不得改变最终分数:{} vs {}",
            left.score,
            right.score
        );
    }
    db.close().expect("close");
}
