//! L1 内存引擎形式化契约验收测试。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-MEM-PRE-001/002/003/004、FC-MEM-POST-001..007、FC-MEM-INV-001..003、FC-MEM-STA-001
//! * FC-MEM-ERR-001/002、FC-MEM-CPLX-001..005(语义哨兵)、FC-GLOBAL-PRE-001..004
//! * FC-GLOBAL-ERR-001、FC-INDEX-POST-003/004、FC-QUERY-ERR-002、FC-QUERY-POST-001
//! * FC-MODEL-INV-022/024/025/026、FC-MODEL-POST-001/002/003/005/006、FC-MODEL-STA-001
//! * FC-SCORE-INV-027、FC-SCORE-POST-001、FC-LIFE-INV-009/023、FC-LIFE-POST-001/002
//!
//! 复杂度/公式类契约的操作计数单测位于源码内(`src/memory/{search,table,lifecycle}.rs`),
//! 见 `contracts.md` §9.2.2 与对应 FC 条目。

use std::sync::{Arc, Mutex};

use mneme::{
    Clock, Dedup, Diversity, Expr, Feedback, InsertMode, InsertOutcome, Limits, Metric, Mneme,
    Record, RelationKind, Retention, Scoring, UpdateOutcome, UpdatePatch,
};
use proptest::prelude::*;

/// 可注入的假时钟(毫秒)。
#[derive(Clone, Default)]
struct FakeClock(Arc<Mutex<i64>>);

impl FakeClock {
    fn set(&self, ms: i64) {
        *self.0.lock().expect("clock lock") = ms;
    }
}

impl Clock for FakeClock {
    fn now_unix_ms(&self) -> i64 {
        *self.0.lock().expect("clock lock")
    }
}

fn mem(dim: u32) -> Mneme {
    Mneme::in_memory(dim).expect("in_memory")
}

fn inserted(outcome: InsertOutcome) -> mneme::RowId {
    match outcome {
        InsertOutcome::Inserted(id) | InsertOutcome::Merged(id) => id,
        other => panic!("期望写入,得到 {other:?}"),
    }
}

fn reference_dot(query: &[f32], records: &[(u64, Vec<f32>)], k: usize) -> Vec<u64> {
    let mut scored: Vec<(f32, u64)> = records
        .iter()
        .map(|(id, vector)| (mneme::simd::dot(query, vector), *id))
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .expect("finite scores")
            .then(a.1.cmp(&b.1))
    });
    scored.truncate(k);
    scored.into_iter().map(|(_, id)| id).collect()
}

/// FC-MEM-PRE-001 / FC-GLOBAL-PRE-001 / FC-GLOBAL-PRE-002
#[test]
fn insert_rejects_dimension_and_non_finite() {
    let ns = mem(3).namespace("n");
    assert!(matches!(
        ns.insert(Record::new(vec![1.0, 2.0])),
        Err(mneme::MnemeError::DimensionMismatch {
            expected: 3,
            got: 2
        })
    ));
    assert!(matches!(
        ns.insert(Record::new(vec![f32::NAN, 0.0, 0.0])),
        Err(mneme::MnemeError::NonFinite)
    ));
    assert!(matches!(
        ns.insert(Record::new(vec![f32::INFINITY, 0.0, 0.0])),
        Err(mneme::MnemeError::NonFinite)
    ));
    assert_eq!(ns.count(None).expect("count"), 0, "库内不得被污染");
}

/// FC-MEM-PRE-002 / FC-GLOBAL-PRE-003
#[test]
fn write_limits_reject_too_large() {
    let limits = Limits {
        key_bytes: 4,
        text_bytes: 4,
        ..Limits::default()
    };
    let db = Mneme::builder()
        .dimension(2)
        .limits(limits)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    assert!(matches!(
        ns.insert(Record::new(vec![0.0, 0.0]).key("toolong")),
        Err(mneme::MnemeError::TooLarge { field: "key", .. })
    ));
    assert!(matches!(
        ns.insert(Record::new(vec![0.0, 0.0]).text("toolong")),
        Err(mneme::MnemeError::TooLarge { field: "text", .. })
    ));
}

/// FC-MEM-PRE-003 / FC-MEM-POST-006 / FC-GLOBAL-PRE-004
#[test]
fn importance_and_confidence_clamped() {
    let ns = mem(2).namespace("n");
    ns.insert(
        Record::new(vec![1.0, 0.0])
            .key("k")
            .importance(5.0)
            .confidence(-1.0),
    )
    .expect("insert");
    let rec = ns.get("k").expect("get").expect("present");
    assert_eq!(rec.importance(), 1.0);
    assert_eq!(rec.confidence(), 0.0);
}

/// FC-MEM-POST-001 / FC-MEM-INV-001 / FC-MEM-INV-002 / FC-MODEL-INV-022
#[test]
fn upsert_keeps_rowid_and_rejects_duplicate() {
    let ns = mem(2).namespace("n");
    let first = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).key("k"))
            .expect("insert"),
    );
    let second = inserted(
        ns.insert(Record::new(vec![0.0, 1.0]).key("k"))
            .expect("upsert"),
    );
    assert_eq!(first, second, "upsert 必须保留 RowId");
    assert_eq!(
        ns.get("k").expect("get").expect("present").vector(),
        &[0.0, 1.0]
    );

    let db = Mneme::builder()
        .dimension(2)
        .insert_mode(InsertMode::RejectDuplicate)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("k"))
        .expect("insert");
    assert!(matches!(
        ns.insert(Record::new(vec![1.0, 0.0]).key("k")),
        Err(mneme::MnemeError::DuplicateKey(_))
    ));
}

/// FC-MEM-POST-002(批量原子 + 逐条重复)
#[test]
fn insert_batch_is_atomic_with_per_row_duplicates() {
    let db = Mneme::builder()
        .dimension(2)
        .insert_mode(InsertMode::RejectDuplicate)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    // 任一条校验失败 → 整批拒绝。
    let batch = vec![
        Record::new(vec![1.0, 0.0]).key("a"),
        Record::new(vec![1.0, 0.0, 0.0]).key("bad"),
    ];
    assert!(ns.insert_batch(batch).is_err());
    assert_eq!(ns.count(None).expect("count"), 0, "不得部分写入");

    // 逐条 RejectDuplicate 不回滚整批。
    ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
        .expect("seed");
    let outcomes = ns
        .insert_batch(vec![
            Record::new(vec![1.0, 0.0]).key("a"),
            Record::new(vec![0.0, 1.0]).key("b"),
        ])
        .expect("batch");
    assert!(matches!(outcomes[0], InsertOutcome::Duplicate { .. }));
    assert!(matches!(outcomes[1], InsertOutcome::Inserted(_)));
    assert!(ns.exists("b").expect("exists"));
}

/// FC-MEM-POST-003 / FC-MODEL-INV-024
#[test]
fn update_is_atomically_visible() {
    let ns = mem(2).namespace("n");
    let id = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).key("k").text("old"))
            .expect("insert"),
    );
    let outcome = ns
        .update("k", UpdatePatch::new().text(Some("new".to_string())))
        .expect("update");
    assert_eq!(outcome, UpdateOutcome::Updated(id));
    let rec = ns.get("k").expect("get").expect("present");
    assert_eq!(rec.rowid(), id);
    assert_eq!(rec.text(), Some("new"));
}

/// FC-MEM-POST-004 / FC-LIFE-INV-009 / FC-MEM-CPLX-004
#[test]
fn delete_hides_records_from_reads() {
    let ns = mem(2).namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("k"))
        .expect("insert");
    assert!(ns.delete("k").expect("delete"));
    assert!(ns.get("k").expect("get").is_none());
    assert_eq!(ns.count(None).expect("count"), 0);
    assert!(
        ns.search()
            .vector(&[1.0, 0.0])
            .execute()
            .expect("search")
            .is_empty()
    );
    let audited = ns
        .iter_with(None, true)
        .expect("iter_with")
        .collect::<Vec<_>>();
    assert_eq!(audited.len(), 1, "审计入口应可见墓碑");
}

/// FC-MEM-POST-005
#[test]
fn batch_point_reads_preserve_order_and_count() {
    let ns = mem(2).namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a").importance(0.9))
        .expect("insert");
    ns.insert(Record::new(vec![0.0, 1.0]).key("b").importance(0.1))
        .expect("insert");
    let many = ns.get_many(&["b", "missing", "a"]).expect("get_many");
    assert_eq!(many.len(), 3);
    assert!(many[0].is_some());
    assert!(many[1].is_none());
    assert!(many[2].is_some());
    let filter = Expr::field("importance").gt(0.5_f32);
    assert_eq!(ns.count(Some(filter)).expect("count"), 1);
}

/// FC-MEM-POST-006
#[test]
fn ttl_is_converted_to_expires_at() {
    let clock = FakeClock::default();
    clock.set(1_000);
    let db = Mneme::builder()
        .dimension(2)
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(
        Record::new(vec![1.0, 0.0])
            .key("k")
            .ttl(std::time::Duration::from_millis(500)),
    )
    .expect("insert");
    assert_eq!(
        ns.get("k").expect("get").expect("present").expires_at(),
        Some(1_500)
    );
    clock.set(1_500);
    assert!(ns.get("k").expect("get").is_none(), "到期即不可见");
}

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

/// FC-MEM-POST-007 / FC-INDEX-POST-004
#[test]
fn dedup_reject_replace_and_merge() {
    // Reject
    let db = Mneme::builder()
        .dimension(2)
        .dedup(Dedup::Reject)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let first = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).text("same"))
            .expect("insert"),
    );
    assert!(matches!(
        ns.insert(Record::new(vec![1.0, 0.0]).text("same")).expect("insert"),
        InsertOutcome::Duplicate { existing, .. } if existing == first
    ));

    // Replace
    let db = Mneme::builder()
        .dimension(2)
        .dedup(Dedup::Replace)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let old = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).text("same"))
            .expect("insert"),
    );
    let new = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).text("same"))
            .expect("insert"),
    );
    assert_ne!(old, new, "Replace 生成新 RowId");
    assert!(ns.get_by_rowid(old).expect("get").is_none(), "旧行被墓碑");

    // Merge
    fn merge(_existing: &mneme::RecordRef<'_>, incoming: &mneme::RecordRef<'_>) -> Option<Record> {
        Some(incoming.to_record())
    }
    let db = Mneme::builder()
        .dimension(2)
        .dedup(Dedup::Merge(merge))
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let base = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).text("a"))
            .expect("insert"),
    );
    let outcome = ns
        .insert(Record::new(vec![1.0, 0.0]).text("b"))
        .expect("insert");
    assert!(matches!(outcome, InsertOutcome::Merged(id) if id == base));
    assert_eq!(
        ns.get_by_rowid(base).expect("get").expect("present").text(),
        Some("b")
    );
}

/// FC-MEM-POST-007(`Replace` 即使带同 key 也生成新 RowId)
#[test]
fn dedup_replace_with_key_gets_new_rowid() {
    let db = Mneme::builder()
        .dimension(2)
        .dedup(Dedup::Replace)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let old = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).key("k").text("same"))
            .expect("insert"),
    );
    let new = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).key("k").text("same"))
            .expect("insert"),
    );
    assert_ne!(old, new);
    assert_eq!(
        ns.get("k").expect("get").expect("present").rowid(),
        new,
        "key 索引应指向新行"
    );
    assert!(ns.get_by_rowid(old).expect("get").is_none());
}

/// FC-MODEL-POST-001 / FC-MODEL-INV-025
#[test]
fn relate_is_idempotent_and_dangling_edges_hidden() {
    let ns = mem(2).namespace("n");
    let a = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("insert"),
    );
    let b = inserted(
        ns.insert(Record::new(vec![0.0, 1.0]).key("b"))
            .expect("insert"),
    );
    ns.relate(a, b, RelationKind::SUPPORTS, 0.5)
        .expect("relate");
    ns.relate(a, b, RelationKind::SUPPORTS, 0.9)
        .expect("relate");
    let edges = ns
        .neighbors(a, &[RelationKind::SUPPORTS])
        .expect("neighbors");
    assert_eq!(edges.len(), 1, "同 (from,to,kind) 幂等");
    assert_eq!(edges[0].weight, 0.9, "重复 relate 覆盖 weight");

    ns.delete("b").expect("delete");
    assert!(
        ns.neighbors(a, &[RelationKind::SUPPORTS])
            .expect("neighbors")
            .is_empty(),
        "悬挂边不可见"
    );
}

/// FC-MODEL-POST-005
#[test]
fn predecessors_returns_incoming_edges() {
    let ns = mem(2).namespace("n");
    let a = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("insert"),
    );
    let b = inserted(
        ns.insert(Record::new(vec![0.0, 1.0]).key("b"))
            .expect("insert"),
    );
    ns.relate(a, b, RelationKind::RELATED, 0.7).expect("relate");
    let incoming = ns.predecessors(b, &[RelationKind::RELATED]).expect("preds");
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].from, a);
    assert!(
        ns.predecessors(a, &[RelationKind::RELATED])
            .expect("preds")
            .is_empty()
    );
}

/// FC-MODEL-POST-003
#[test]
fn supersede_closes_previous_valid_to() {
    let clock = FakeClock::default();
    clock.set(1_000);
    let db = Mneme::builder()
        .dimension(2)
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(
        Record::new(vec![1.0, 0.0])
            .key("os")
            .valid_from(100)
            .text("windows"),
    )
    .expect("insert");
    clock.set(2_000);
    ns.supersede(
        "os",
        Record::new(vec![0.0, 1.0]).valid_from(200).text("macos"),
    )
    .expect("supersede");

    let historical = db.as_of(1_500).expect("as_of");
    let snap_ns = historical.namespace("n");
    let old = snap_ns.get("os").expect("get").expect("present");
    assert_eq!(old.text(), Some("windows"));
    assert_eq!(old.valid_to(), Some(200), "旧版本 valid_to 被闭合");
}

/// FC-MODEL-INV-026
#[test]
fn as_of_returns_historical_snapshot() {
    let clock = FakeClock::default();
    clock.set(1_000);
    let db = Mneme::builder()
        .dimension(2)
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("k").text("v1"))
        .expect("insert");
    clock.set(2_000);
    ns.update("k", UpdatePatch::new().text(Some("v2".to_string())))
        .expect("update");

    let historical = db.as_of(1_500).expect("as_of");
    assert_eq!(
        historical
            .namespace("n")
            .get("k")
            .expect("get")
            .expect("present")
            .text(),
        Some("v1")
    );
    assert_eq!(
        ns.get("k").expect("get").expect("present").text(),
        Some("v2")
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

/// FC-LIFE-INV-023 / FC-LIFE-POST-001
#[test]
fn retain_forgets_below_threshold() {
    let ns = mem(2).namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("keep").importance(1.0))
        .expect("insert");
    let forgotten_id = inserted(
        ns.insert(Record::new(vec![0.0, 1.0]).key("drop").importance(0.0))
            .expect("insert"),
    );
    let report = ns.retain(Retention::new()).expect("retain");
    assert_eq!(report.forgotten, 1);
    assert_eq!(report.sampled_ids, vec![forgotten_id]);
    assert!(ns.exists("keep").expect("exists"));
    assert!(!ns.exists("drop").expect("exists"));
}

/// FC-MODEL-POST-002
#[test]
fn consolidate_merges_cluster_and_keeps_sources() {
    let ns = mem(2).namespace("n");
    for (index, vector) in [
        vec![1.0, 0.0],
        vec![0.99, 0.01],
        vec![0.98, 0.02],
        vec![0.0, 1.0],
    ]
    .iter()
    .enumerate()
    {
        ns.insert(
            Record::new(vector.clone())
                .key(format!("k{index}"))
                .text(format!("t{index}")),
        )
        .expect("insert");
    }
    let report = ns
        .consolidate(mneme::ConsolidationPolicy {
            threshold: 0.9,
            keep_sources: true,
            ..Default::default()
        })
        .expect("consolidate");
    assert_eq!(report.clusters, 1);
    assert_eq!(report.merged, 3);
    assert_eq!(report.created.len(), 1);
    assert!(ns.exists("k0").expect("exists"), "keep_sources 保留来源");
}

/// FC-MEM-ERR-002(延后能力返回结构化错误)
#[test]
fn deferred_features_return_structured_errors() {
    let db = mem(2);
    let ns = db.namespace("n");
    assert!(matches!(
        ns.search().text("x").execute(),
        Err(mneme::MnemeError::Unsupported { .. })
    ));
    assert!(matches!(
        Mneme::open("./nowhere"),
        Err(mneme::MnemeError::Unsupported { .. })
    ));
    assert!(matches!(
        db.backup_to("./nowhere"),
        Err(mneme::MnemeError::Unsupported { .. })
    ));
    assert!(matches!(
        Mneme::builder().dimension(2).path("./x").build(),
        Err(mneme::MnemeError::Unsupported { .. })
    ));
}

/// FC-MEM-ERR-001
#[test]
fn closed_database_rejects_operations() {
    let db = mem(2);
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("k"))
        .expect("insert");
    db.close().expect("close");
    assert!(matches!(
        ns.insert(Record::new(vec![1.0, 0.0]).key("j")),
        Err(mneme::MnemeError::Closed)
    ));
    assert!(matches!(ns.get("k"), Err(mneme::MnemeError::Closed)));
}

/// FC-GLOBAL-ERR-001
#[test]
fn l1_api_smoke_never_panics() {
    let ns = mem(2).namespace("n");
    let _ = ns.get("missing").expect("get");
    let _ = ns.get_many(&[]).expect("get_many");
    let _ = ns.count(Some(Expr::Always)).expect("count");
    let _ = ns
        .search()
        .vector(&[1.0, 0.0])
        .filter(Expr::Never)
        .top_k(0)
        .diversify(Diversity::Mmr { lambda: 0.5 })
        .execute()
        .expect("search");
    let _ = ns.iter(None).expect("iter").count();
    let _ = ns.touch("missing", Some(1.0)).expect("touch");
    let _ = ns.delete("missing").expect("delete");
    let _ = ns
        .neighbors(mneme::RowId::new(999), &[])
        .expect("neighbors");
}

/// FC-MEM-PRE-002 / FC-GLOBAL-PRE-003(metadata 字节与深度限额)
#[test]
fn write_meta_limits_reject_too_large() {
    // meta 序列化字节超限 → TooLarge
    let limits = Limits {
        meta_bytes: 8,
        ..Limits::default()
    };
    let db = Mneme::builder()
        .dimension(2)
        .limits(limits)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    assert!(matches!(
        ns.insert(Record::new(vec![0.0, 0.0]).metadata(mneme::json!({"k": "a-long-string"}))),
        Err(mneme::MnemeError::TooLarge {
            field: "metadata",
            ..
        })
    ));
    // meta 嵌套深度超限 → MetaTooDeep
    let limits = Limits {
        meta_depth: 1,
        ..Limits::default()
    };
    let db = Mneme::builder()
        .dimension(2)
        .limits(limits)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    assert!(matches!(
        ns.insert(Record::new(vec![0.0, 0.0]).metadata(mneme::json!({"a": {"b": {"c": 1}}}))),
        Err(mneme::MnemeError::MetaTooDeep { .. })
    ));
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

/// FC-GLOBAL-PRE-003(key 限额 Bound-1 / Bound / Bound+1 三点采样)
#[test]
fn write_limits_boundary_three_point() {
    let limits = Limits {
        key_bytes: 4,
        ..Limits::default()
    };
    let db = Mneme::builder()
        .dimension(2)
        .limits(limits)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    assert!(ns.insert(Record::new(vec![0.0, 0.0]).key("abc")).is_ok());
    assert!(ns.insert(Record::new(vec![0.0, 0.0]).key("abcd")).is_ok());
    assert!(matches!(
        ns.insert(Record::new(vec![0.0, 0.0]).key("abcde")),
        Err(mneme::MnemeError::TooLarge {
            field: "key",
            limit: 4,
            got: 5
        })
    ));
}

/// FC-GLOBAL-PRE-004 / FC-MEM-PRE-003(importance/confidence 钳制三点采样)
#[test]
fn importance_confidence_boundary_three_point() {
    let ns = mem(2).namespace("n");
    ns.insert(
        Record::new(vec![1.0, 0.0])
            .key("bound")
            .importance(0.0)
            .confidence(1.0),
    )
    .expect("insert");
    let bound = ns.get("bound").expect("get").expect("present");
    assert_eq!(bound.importance(), 0.0, "Bound 原样保留");
    assert_eq!(bound.confidence(), 1.0);

    ns.insert(
        Record::new(vec![0.0, 1.0])
            .key("under")
            .importance(-0.1)
            .confidence(-0.1),
    )
    .expect("insert");
    let under = ns.get("under").expect("get").expect("present");
    assert_eq!(under.importance(), 0.0, "Bound-1 钳到下界");
    assert_eq!(under.confidence(), 0.0);

    ns.insert(
        Record::new(vec![1.0, 1.0])
            .key("over")
            .importance(1.1)
            .confidence(1.1),
    )
    .expect("insert");
    let over = ns.get("over").expect("get").expect("present");
    assert_eq!(over.importance(), 1.0, "Bound+1 钳到上界");
    assert_eq!(over.confidence(), 1.0);
}

/// FC-MODEL-STA-001 / FC-MODEL-INV-024 / FC-MODEL-INV-026(版本状态机)
#[test]
fn model_version_lifecycle_states() {
    let clock = FakeClock::default();
    clock.set(1_000);
    let db = Mneme::builder()
        .dimension(2)
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let id = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).key("k").text("v1"))
            .expect("insert"),
    );
    // Active:当前读可见
    assert_eq!(
        ns.get("k").expect("get").expect("present").text(),
        Some("v1")
    );

    // Update:旧版本 → Shadowed,新版本 Active(δ 转移)
    clock.set(2_000);
    ns.update("k", UpdatePatch::new().text(Some("v2".to_string())))
        .expect("update");
    assert_eq!(
        ns.get("k").expect("get").expect("present").text(),
        Some("v2")
    );

    // 非法转移拦截:Shadowed 不得出现在当前读路径
    let current: Vec<u64> = ns
        .search()
        .vector(&[1.0, 0.0])
        .execute()
        .expect("search")
        .iter()
        .map(|hit| hit.rowid.get())
        .collect();
    assert_eq!(current, vec![id.get()], "当前读只返回 Active 版本");

    // Shadowed 经 as_of 历史读可见(合法转移)
    let historical = db.as_of(1_500).expect("as_of");
    assert_eq!(
        historical
            .namespace("n")
            .get("k")
            .expect("get")
            .expect("present")
            .text(),
        Some("v1")
    );

    // Delete:Active → Shadowed,当前读彻底不可见
    clock.set(3_000);
    assert!(ns.delete("k").expect("delete"));
    assert!(ns.get("k").expect("get").is_none());
    assert!(
        ns.search()
            .vector(&[1.0, 0.0])
            .execute()
            .expect("search")
            .is_empty()
    );
}

/// FC-MEM-STA-001 / FC-MEM-ERR-001(库生命周期 Open → Closed,Closed → Closed 幂等)
#[test]
fn database_lifecycle_open_closed() {
    let db = mem(2);
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("k"))
        .expect("insert");
    let clone = db.clone();
    db.close().expect("close");
    assert!(matches!(ns.get("k"), Err(mneme::MnemeError::Closed)));
    assert!(matches!(
        ns.insert(Record::new(vec![1.0, 0.0])),
        Err(mneme::MnemeError::Closed)
    ));
    // Closed 上再次 Close(自环)幂等返回 Ok
    clone.close().expect("close idempotent");
}

/// §0.2 错误分类矩阵(变体语义互不混淆)
#[test]
fn error_taxonomy_is_specific() {
    let ns = mem(2).namespace("n");
    assert!(matches!(
        ns.insert(Record::new(vec![f32::NAN, 0.0])),
        Err(mneme::MnemeError::NonFinite)
    ));
    assert!(matches!(
        ns.insert(Record::new(vec![1.0])),
        Err(mneme::MnemeError::DimensionMismatch { .. })
    ));
    assert!(matches!(
        Mneme::open("./nowhere"),
        Err(mneme::MnemeError::Unsupported { .. })
    ));
    assert!(matches!(
        Mneme::builder().dimension(2).path("./x").build(),
        Err(mneme::MnemeError::Unsupported { .. })
    ));
    assert!(matches!(
        Mneme::builder().build(),
        Err(mneme::MnemeError::Config { .. })
    ));
    assert!(matches!(
        mneme::Dimension::new(0),
        Err(mneme::MnemeError::LimitExceeded { .. })
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

/// FC-MODEL-INV-024(I24 原子可见:并发读者不得看到字段混合的半更新)
#[test]
fn concurrent_readers_never_see_torn_updates() {
    let db = mem(2);
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![0.0, 1.0]).key("k").text("v0"))
        .expect("insert");
    let reader_db = db.clone();
    let reader = std::thread::spawn(move || {
        let rns = reader_db.namespace("n");
        for _ in 0..2_000 {
            if let Some(rec) = rns.get("k").expect("get") {
                let version = rec.vector()[0] as u32;
                assert_eq!(
                    rec.text(),
                    Some(format!("v{version}").as_str()),
                    "读到字段混合的半更新"
                );
            }
        }
    });
    for version in 1..200_u32 {
        ns.update(
            "k",
            UpdatePatch::new()
                .vector(vec![version as f32, 1.0])
                .text(Some(format!("v{version}"))),
        )
        .expect("update");
    }
    reader.join().expect("reader");
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

    /// FC-MEM-INV-001 / FC-MEM-INV-002(RowId 稳定 + SeqNo 水位单调)
    #[test]
    fn seqno_and_rowid_stable_prop(texts in prop::collection::vec("[a-z]{1,12}", 1..16)) {
        let db = mem(2);
        let ns = db.namespace("n");
        let id = inserted(ns.insert(Record::new(vec![1.0, 0.0]).key("k")).expect("insert"));
        let mut last_version = db.snapshot().version();
        for text in texts {
            ns.update("k", UpdatePatch::new().text(Some(text))).expect("update");
            prop_assert_eq!(ns.get("k").expect("get").expect("present").rowid(), id);
            let version = db.snapshot().version();
            prop_assert!(version > last_version, "SeqNo 必须严格单调递增");
            last_version = version;
        }
    }

    /// FC-MODEL-POST-001(relate 以 (from,to,kind) 幂等,重复覆盖 weight)
    #[test]
    fn relate_is_idempotent_prop(weights in prop::collection::vec(0.0f32..1.0, 1..8)) {
        let ns = mem(2).namespace("n");
        let a = inserted(ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("insert"));
        let b = inserted(ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("insert"));
        for weight in &weights {
            ns.relate(a, b, RelationKind::SUPPORTS, *weight).expect("relate");
        }
        let edges = ns.neighbors(a, &[RelationKind::SUPPORTS]).expect("neighbors");
        prop_assert_eq!(edges.len(), 1);
        prop_assert_eq!(edges[0].weight, *weights.last().expect("non-empty"));
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

    /// FC-MODEL-INV-026(as_of 取事务时间 ≤ t 的最新版本)
    #[test]
    fn as_of_matches_reference_prop(
        versions in prop::collection::vec("[a-z]{1,8}", 2..8),
        cut in 0usize..8,
    ) {
        let clock = FakeClock::default();
        let db = Mneme::builder()
            .dimension(2)
            .clock(Arc::new(clock.clone()))
            .build()
            .expect("build");
        let ns = db.namespace("n");
        let mut history = Vec::new();
        for (index, text) in versions.iter().enumerate() {
            let ts = 1_000 + index as i64 * 1_000;
            clock.set(ts);
            if index == 0 {
                ns.insert(Record::new(vec![1.0, 0.0]).key("k").text(text.clone()))
                    .expect("insert");
            } else {
                ns.update("k", UpdatePatch::new().text(Some(text.clone())))
                    .expect("update");
            }
            history.push((ts, text.clone()));
        }
        let cut = cut.min(history.len() - 1);
        let snapshot = db.as_of(history[cut].0).expect("as_of");
        let snapshot_ns = snapshot.namespace("n");
        let record = snapshot_ns.get("k").expect("get").expect("present");
        prop_assert_eq!(record.text(), Some(history[cut].1.as_str()));
    }
}
