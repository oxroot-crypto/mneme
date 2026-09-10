//! L1 写入 / 更新 / 删除 / 去重 / 限额契约验收测试。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-MEM-PRE-001/002/003、FC-MEM-POST-001..007、FC-MEM-INV-001..003
//! * FC-GLOBAL-PRE-001..004
//!
//! 检索/过滤/打分(FC-QUERY-*/FC-INDEX-*/FC-SCORE-*)、记忆模型(FC-MODEL-*)、
//! 遗忘与库生命周期(FC-LIFE-*、FC-MEM-ERR/STA)分见 `query_contracts.rs`、
//! `model_contracts.rs`、`life_contracts.rs`。复杂度/公式类契约的操作计数单测位于
//! 源码内(`src/memory/{search,table,lifecycle}.rs`),见 `contracts.md` §9.2.2。

use std::sync::Arc;

use mneme::{
    Dedup, Expr, InsertMode, InsertOutcome, Limits, Mneme, Record, UpdateOutcome, UpdatePatch,
};
use proptest::prelude::*;

mod common;

use common::{FakeClock, inserted, mem};

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
    assert!(matches!(
        ns.insert_batch(batch),
        Err(mneme::MnemeError::DimensionMismatch { .. })
    ));
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

proptest! {
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
}
