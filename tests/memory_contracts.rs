//! L1 写入 / 更新 / 删除 / 去重 / 限额契约验收测试。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-MEM-PRE-001/002/003、FC-MEM-POST-001..007/009、FC-MEM-INV-001/002
//! * FC-GLOBAL-PRE-001..004、FC-MEM-CPLX-004(delete 复杂度哨兵)
//! * 跨族条目:FC-INDEX-POST-004、FC-MODEL-INV-022/024、FC-LIFE-INV-009
//!
//! 检索/过滤/打分、记忆模型、遗忘与库生命周期的专项测试分见
//! `query_contracts.rs`、`model_contracts.rs`、`life_contracts.rs`。复杂度/公式类
//! 契约的操作计数单测位于源码内(`src/memory/{search,table,lifecycle}.rs`),见
//! `contracts.md` §9.2.2,不属本文件的覆盖声明范围。

use std::sync::Arc;

use mneme::{
    Dedup, Expr, InsertMode, InsertOutcome, Limits, Mneme, Record, RowId, UpdateOutcome,
    UpdatePatch,
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

    // 非有限值(NaN)拒绝而非静默钳制:NaN 会污染综合打分与遗忘公式。
    assert!(matches!(
        ns.insert(Record::new(vec![1.0, 0.0]).key("nan").importance(f32::NAN)),
        Err(mneme::MnemeError::NonFinite)
    ));
    assert!(matches!(
        ns.insert(Record::new(vec![1.0, 0.0]).key("nan2").confidence(f32::NAN)),
        Err(mneme::MnemeError::NonFinite)
    ));
    assert!(!ns.exists("nan").expect("exists"), "NaN 不得入库");
    // touch boost 同口径:拒绝且访问统计不被污染。
    assert!(matches!(
        ns.touch("k", Some(f32::NAN)),
        Err(mneme::MnemeError::NonFinite)
    ));
}

/// FC-MEM-PRE-002 / FC-GLOBAL-PRE-003(更新补丁与 insert 同限额口径)
#[test]
fn update_enforces_write_limits() {
    // 字节限额:text / metadata 超限 → TooLarge
    let limits = Limits {
        text_bytes: 4,
        meta_bytes: 8,
        ..Limits::default()
    };
    let db = Mneme::builder()
        .dimension(2)
        .limits(limits)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("k").text("old"))
        .expect("insert");

    assert!(matches!(
        ns.update("k", UpdatePatch::new().text(Some("toolong".to_string()))),
        Err(mneme::MnemeError::TooLarge { field: "text", .. })
    ));
    assert!(matches!(
        ns.update(
            "k",
            UpdatePatch::new().metadata(Some(mneme::json!({"k": "a-long-string"})))
        ),
        Err(mneme::MnemeError::TooLarge {
            field: "metadata",
            ..
        })
    ));
    // importance 含非有限值 → NonFinite
    assert!(matches!(
        ns.update("k", UpdatePatch::new().importance(f32::NAN)),
        Err(mneme::MnemeError::NonFinite)
    ));
    // 更新失败不得产生任何部分写入:记录保持上一版本原样。
    let rec = ns.get("k").expect("get").expect("present");
    assert_eq!(rec.text(), Some("old"));
    assert_eq!(rec.importance(), 0.5, "缺省重要度未被污染");

    // 深度限额:metadata 嵌套超限 → MetaTooDeep
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
    ns.insert(Record::new(vec![1.0, 0.0]).key("k"))
        .expect("insert");
    assert!(matches!(
        ns.update(
            "k",
            UpdatePatch::new().metadata(Some(mneme::json!({"a": {"b": 1}})))
        ),
        Err(mneme::MnemeError::MetaTooDeep { .. })
    ));

    // 边界值恰好等于上限时接受(Bound):text_bytes = 4 下 "abcd" 合法。
    let limits = Limits {
        text_bytes: 4,
        ..Limits::default()
    };
    let db = Mneme::builder()
        .dimension(2)
        .limits(limits)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("k"))
        .expect("insert");
    ns.update("k", UpdatePatch::new().text(Some("abcd".to_string())))
        .expect("update");
    assert_eq!(
        ns.get("k").expect("get").expect("present").text(),
        Some("abcd")
    );
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

/// FC-MEM-POST-001 / FC-LIFE-INV-009
/// `InsertMode::RejectDuplicate` 只拒绝同 key 的**可见**重复;墓碑/逻辑过期记录
/// 视为不存在,复用既有 `RowId`(与 `exists`/`Dedup::Reject` 同口径)。
#[test]
fn reject_duplicate_ignores_expired_and_deleted() {
    let clock = Arc::new(FakeClock::default());
    clock.set(1_000);
    let db = Mneme::builder()
        .dimension(2)
        .insert_mode(InsertMode::RejectDuplicate)
        .clock(clock.clone())
        .build()
        .expect("build");
    let ns = db.namespace("n");
    // 逻辑过期:不再拒绝同 key 写入,且复用原 RowId。
    let first = inserted(
        ns.insert(
            Record::new(vec![1.0, 0.0])
                .key("e")
                .ttl(std::time::Duration::from_millis(500)),
        )
        .expect("insert"),
    );
    clock.set(2_000);
    assert!(!ns.exists("e").expect("exists"), "TTL 后不可见");
    let second = inserted(
        ns.insert(Record::new(vec![0.0, 1.0]).key("e"))
            .expect("逻辑过期记录不应拒绝重复"),
    );
    assert_eq!(first, second, "逻辑过期记录视为不存在,复用 RowId");
    // 墓碑:同样不拒绝,复用 RowId。
    ns.delete("e").expect("delete");
    let third = inserted(
        ns.insert(Record::new(vec![1.0, 1.0]).key("e"))
            .expect("墓碑记录不应拒绝重复"),
    );
    assert_eq!(first, third, "墓碑记录视为不存在,复用 RowId");
    // 可见记录仍正常拒绝。
    assert!(matches!(
        ns.insert(Record::new(vec![1.0, 0.0]).key("e")),
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

    // 预校验后仍失败(Dedup::Merge 回调产物超限)→ 整批回滚;且残留版本不得在
    // 下一次 publish 时复活(FC-MEM-POST-002)。
    fn merge_oversized(
        _existing: &mneme::RecordRef<'_>,
        _incoming: &mneme::RecordRef<'_>,
    ) -> Option<Record> {
        Some(Record::new(vec![1.0, 0.0]).text("toolong"))
    }
    let db = Mneme::builder()
        .dimension(2)
        .limits(Limits {
            text_bytes: 4,
            ..Limits::default()
        })
        .dedup(Dedup::Merge(merge_oversized))
        .build()
        .expect("build");
    let ns = db.namespace("n");
    assert!(matches!(
        ns.insert_batch(vec![
            Record::new(vec![1.0, 0.0]).text("aaaa"),
            Record::new(vec![1.0, 0.0]).text("aaaa"),
        ]),
        Err(mneme::MnemeError::TooLarge { field: "text", .. })
    ));
    assert_eq!(ns.count(None).expect("count"), 0, "失败整批不得残留");
    // 后续成功写入触发 publish:回滚必须已清除批内残留,count 只能为 1。
    ns.insert(Record::new(vec![0.0, 1.0]).text("bbbb"))
        .expect("insert");
    assert_eq!(
        ns.count(None).expect("count"),
        1,
        "回滚后不得复活批内残留版本"
    );
}

/// FC-MEM-POST-002(失败的单条与批量写入均不登记命名空间)
#[test]
fn failed_writes_do_not_register_namespace() {
    fn merge_oversized(
        _existing: &mneme::RecordRef<'_>,
        _incoming: &mneme::RecordRef<'_>,
    ) -> Option<Record> {
        Some(Record::new(vec![1.0, 0.0]).text("toolong"))
    }
    let db = Mneme::builder()
        .dimension(2)
        .limits(Limits {
            text_bytes: 4,
            ..Limits::default()
        })
        .dedup(Dedup::Merge(merge_oversized))
        .build()
        .expect("build");
    // 单条 insert 预校验失败 → 不登记。
    assert!(
        db.namespace("single")
            .insert(Record::new(vec![1.0]))
            .is_err()
    );
    assert!(db.list_namespaces().expect("list").is_empty());
    // 批量预校验失败 → 不登记。
    assert!(
        db.namespace("batch")
            .insert_batch(vec![Record::new(vec![1.0])])
            .is_err()
    );
    assert!(db.list_namespaces().expect("list").is_empty());
    // 批量预校验后中途失败(Merge 产物超限)→ 回滚全部副作用,含命名空间登记。
    assert!(
        db.namespace("mid")
            .insert_batch(vec![
                Record::new(vec![1.0, 0.0]).text("aaaa"),
                Record::new(vec![1.0, 0.0]).text("aaaa"),
            ])
            .is_err()
    );
    assert!(
        db.list_namespaces().expect("list").is_empty(),
        "失败批不得残留命名空间登记"
    );
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

/// FC-MEM-POST-005(`get_many_by_rowid` 顺序一一对应;未命中与逻辑过期以 None 占位)
#[test]
fn batch_rowid_reads_preserve_order() {
    let clock = FakeClock::default();
    clock.set(1_000);
    let db = Mneme::builder()
        .dimension(2)
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let id_a = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("insert"),
    );
    let id_b = inserted(
        ns.insert(Record::new(vec![0.0, 1.0]).key("b"))
            .expect("insert"),
    );
    let id_ttl = inserted(
        ns.insert(
            Record::new(vec![1.0, 1.0])
                .key("ttl")
                .ttl(std::time::Duration::from_millis(500)),
        )
        .expect("insert"),
    );
    // 顺序与输入一一对应;不存在的 RowId 以 None 占位。
    let reads = ns
        .get_many_by_rowid(&[id_b, RowId::new(999), id_a, id_ttl])
        .expect("get_many_by_rowid");
    assert_eq!(reads.len(), 4);
    assert_eq!(reads[0].as_ref().map(|rec| rec.rowid()), Some(id_b));
    assert!(reads[1].is_none(), "不存在的 RowId 以 None 占位");
    assert_eq!(reads[2].as_ref().map(|rec| rec.rowid()), Some(id_a));
    assert!(reads[3].is_some(), "TTL 未到期仍可见");
    // 逻辑过期后占位 None(I9:常规读路径不可见)。
    clock.set(1_501);
    let reads = ns
        .get_many_by_rowid(&[id_a, id_ttl])
        .expect("get_many_by_rowid");
    assert!(reads[0].is_some());
    assert!(reads[1].is_none(), "已过期记录在批量点读中不可见");
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

/// FC-MEM-POST-009(`touch` 仅对可见记录生效;墓碑/逻辑过期返回 `false`)
#[test]
fn touch_only_affects_visible_records() {
    let clock = FakeClock::default();
    clock.set(1_000);
    let db = Mneme::builder()
        .dimension(2)
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let rowid = inserted(
        ns.insert(
            Record::new(vec![1.0, 0.0])
                .key("k")
                .ttl(std::time::Duration::from_millis(500)),
        )
        .expect("insert"),
    );
    assert!(ns.touch("k", Some(0.1)).expect("touch"), "存活记录可强化");
    clock.set(1_500);
    assert!(
        !ns.touch("k", Some(0.1)).expect("touch"),
        "逻辑过期记录不得被强化"
    );
    assert!(
        !ns.touch_by_rowid(rowid, Some(0.1)).expect("touch"),
        "逻辑过期记录不得被强化"
    );
    ns.delete("k").expect("delete");
    assert!(
        !ns.touch_by_rowid(rowid, None).expect("touch"),
        "墓碑记录不得被强化"
    );
    assert!(!ns.touch("k", None).expect("touch"), "墓碑记录不得被强化");
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

/// FC-MEM-POST-007(merge 回调改变 key 时 key 索引随新版本迁移,不悬挂)
#[test]
fn dedup_merge_key_change_migrates_index() {
    fn merge(_existing: &mneme::RecordRef<'_>, incoming: &mneme::RecordRef<'_>) -> Option<Record> {
        Some(incoming.to_record())
    }
    let db = Mneme::builder()
        .dimension(2)
        .dedup(Dedup::Merge(merge))
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("old"))
        .expect("insert");
    // 同向量 → 命中 merge,回调把 key 改为 "new"。
    let outcome = ns
        .insert(Record::new(vec![1.0, 0.0]).key("new"))
        .expect("insert");
    assert!(matches!(outcome, InsertOutcome::Merged(_)));
    assert!(ns.get("old").expect("get").is_none(), "旧 key 索引应迁移");
    assert_eq!(
        ns.get("new").expect("get").expect("present").key(),
        Some("new")
    );
    assert!(db.check().expect("check").ok, "merge 改 key 后索引不应悬挂");
}

/// FC-MEM-POST-007(合并/替换产物 key 与另一可见记录冲突 → `DuplicateKey` 且整体回滚)
#[test]
fn key_migration_rejects_live_key_conflict() {
    fn merge(_existing: &mneme::RecordRef<'_>, incoming: &mneme::RecordRef<'_>) -> Option<Record> {
        Some(incoming.to_record())
    }
    // Merge:命中相似记录后,回调把 key 改成另一存活记录已占用的 key。
    let db = Mneme::builder()
        .dimension(2)
        .dedup(Dedup::Merge(merge))
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
        .expect("insert a");
    ns.insert(Record::new(vec![0.0, 1.0]).key("b"))
        .expect("insert b");
    assert!(matches!(
        ns.insert(Record::new(vec![1.0, 0.0]).key("b")),
        Err(mneme::MnemeError::DuplicateKey(_))
    ));
    assert_eq!(
        ns.get("b")
            .expect("get b")
            .expect("present")
            .vector()
            .to_vec(),
        vec![0.0, 1.0],
        "b 的向量不得被合并产物覆盖"
    );
    assert!(ns.get("a").expect("get a").is_some());
    assert_eq!(ns.count(None).expect("count"), 2);
    assert!(db.check().expect("check").ok);

    // Replace:相似记录被墓碑后,新行 key 与另一存活记录冲突同样拒绝并整体回滚。
    let db = Mneme::builder()
        .dimension(2)
        .dedup(Dedup::Replace)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
        .expect("insert a");
    ns.insert(Record::new(vec![0.0, 1.0]).key("b"))
        .expect("insert b");
    assert!(matches!(
        ns.insert(Record::new(vec![1.0, 0.0]).key("b")),
        Err(mneme::MnemeError::DuplicateKey(_))
    ));
    assert!(
        ns.get("a").expect("get a").is_some(),
        "失败的 Replace 不得留下墓碑"
    );
    assert_eq!(ns.count(None).expect("count"), 2);
    assert!(db.check().expect("check").ok);
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

/// FC-MEM-PRE-002 / FC-GLOBAL-PRE-003(supersede 与 merge 产物同限额口径,失败零部分写入)
#[test]
fn supersede_and_merge_enforce_write_limits() {
    // supersede:新版本 text 超限 → TooLarge,旧记录保持原样(valid_to 未闭合)。
    let db = Mneme::builder()
        .dimension(2)
        .limits(Limits {
            text_bytes: 4,
            ..Limits::default()
        })
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("k").text("old"))
        .expect("insert");
    assert!(matches!(
        ns.supersede("k", Record::new(vec![0.0, 1.0]).text("toolong")),
        Err(mneme::MnemeError::TooLarge { field: "text", .. })
    ));
    assert_eq!(
        ns.get("k").expect("get").expect("present").text(),
        Some("old"),
        "失败的 supersede 不得部分写入"
    );
    // 合法新版本重试成功。
    ns.supersede("k", Record::new(vec![1.0, 1.0]).text("new"))
        .expect("supersede");
    assert_eq!(
        ns.get("k").expect("get").expect("present").text(),
        Some("new")
    );

    // Dedup::Merge:回调产出的合并记录超限 → TooLarge,原记录不变。
    fn merge_oversized(
        _existing: &mneme::RecordRef<'_>,
        incoming: &mneme::RecordRef<'_>,
    ) -> Option<Record> {
        Some(incoming.to_record().text("toolong"))
    }
    let db = Mneme::builder()
        .dimension(2)
        .limits(Limits {
            text_bytes: 4,
            ..Limits::default()
        })
        .dedup(Dedup::Merge(merge_oversized))
        .build()
        .expect("build");
    let ns = db.namespace("n");
    let base = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).text("a"))
            .expect("insert"),
    );
    assert!(
        matches!(
            ns.insert(Record::new(vec![1.0, 0.0]).text("b")),
            Err(mneme::MnemeError::TooLarge { field: "text", .. })
        ),
        "合并产物超限必须拒绝"
    );
    assert_eq!(
        ns.get_by_rowid(base).expect("get").expect("present").text(),
        Some("a"),
        "合并失败时旧记录保持原样"
    );
}

/// FC-LIFE-INV-009(内部辅助路径同样排除逻辑过期记录:dedup 判重、`stats` 计数、
/// `consolidate` 候选、`forget` 目标)
#[test]
fn logically_expired_hidden_from_internal_paths() {
    let clock = FakeClock::default();
    clock.set(1_000);

    // 1) dedup:过期记录不得参与判重(独立库开启 Dedup::Reject)。
    let dedup_db = Mneme::builder()
        .dimension(2)
        .clock(Arc::new(clock.clone()))
        .dedup(Dedup::Reject)
        .build()
        .expect("build");
    let dns = dedup_db.namespace("n");
    dns.insert(
        Record::new(vec![1.0, 0.0])
            .key("e")
            .text("expired")
            .ttl(std::time::Duration::from_millis(500)),
    )
    .expect("insert");
    clock.set(1_501);
    // 同文本命中过期记录:不得判为重复(改用不同向量,仅文本相同)。
    assert!(matches!(
        dns.insert(Record::new(vec![0.0, 1.0]).key("e2").text("expired")),
        Ok(InsertOutcome::Inserted(_))
    ));

    // 2) stats / forget / consolidate:默认关闭去重,让 E 与 L 同向量共存。
    clock.set(1_000);
    let db = Mneme::builder()
        .dimension(2)
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(
        Record::new(vec![1.0, 0.0])
            .key("e")
            .text("expired")
            .ttl(std::time::Duration::from_millis(500)),
    )
    .expect("insert");
    ns.insert(Record::new(vec![1.0, 0.0]).key("l").text("live"))
        .expect("insert");
    clock.set(1_501);

    // 过期记录不计入 doc_count(仅 L,共 1)。
    let stats = db.stats().expect("stats");
    assert_eq!(
        stats.per_namespace["n"].doc_count, 1,
        "过期记录不得计入 stats"
    );
    // 过期记录已不可见,不计入 forget 命中数。
    assert_eq!(
        ns.forget(Expr::field("key").eq("e")).expect("forget"),
        0,
        "过期记录不得被 forget 命中"
    );
    // 过期记录不得作为聚类候选(E 若入选会与同向量的 L 聚为一簇)。
    let report = ns
        .consolidate(mneme::ConsolidationPolicy {
            threshold: 0.9,
            ..Default::default()
        })
        .expect("consolidate");
    assert_eq!(report.clusters, 0, "过期记录不得进入沉淀候选");
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
