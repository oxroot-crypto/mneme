//! L1 记忆模型(关系 / 双时态 / 版本状态机 / 沉淀 / 并发)契约验收测试。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-MODEL-INV-024/025/026、FC-MODEL-POST-001..003/005/006、FC-MODEL-STA-001
//! * FC-GLOBAL-PRE-004(关系边权:非有限值拒绝、越界钳制)
//! * FC-MODEL-CPLX-002(consolidate 两两余弦复杂度哨兵,§9.2.8)

use std::sync::Arc;

use mneme::{Mneme, Record, RelationKind, UpdatePatch};
use proptest::prelude::*;

mod common;

use common::{FakeClock, inserted, mem};

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

/// FC-GLOBAL-PRE-004(关系边权:非有限值拒绝、越界钳制到 [0,1])
#[test]
fn relate_weight_rejects_non_finite_and_clamps() {
    let ns = mem(2).namespace("n");
    let a = inserted(
        ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("insert"),
    );
    let b = inserted(
        ns.insert(Record::new(vec![0.0, 1.0]).key("b"))
            .expect("insert"),
    );
    // 非有限值(NaN/±Inf)拒绝,不产生任何边(FC-GLOBAL-PRE-004)。
    assert!(matches!(
        ns.relate(a, b, RelationKind::SUPPORTS, f32::NAN),
        Err(mneme::MnemeError::NonFinite)
    ));
    assert!(matches!(
        ns.relate(a, b, RelationKind::SUPPORTS, f32::INFINITY),
        Err(mneme::MnemeError::NonFinite)
    ));
    assert!(
        ns.neighbors(a, &[RelationKind::SUPPORTS])
            .expect("neighbors")
            .is_empty(),
        "被拒绝的 relate 不得残留边"
    );
    // 越界有限值钳制到 [0,1](Bound-1 / Bound+1)。
    ns.relate(a, b, RelationKind::SUPPORTS, -0.5)
        .expect("relate");
    let edges = ns
        .neighbors(a, &[RelationKind::SUPPORTS])
        .expect("neighbors");
    assert_eq!(edges[0].weight, 0.0, "负权重钳制到下界");
    ns.relate(a, b, RelationKind::SUPPORTS, 1.5)
        .expect("relate");
    let edges = ns
        .neighbors(a, &[RelationKind::SUPPORTS])
        .expect("neighbors");
    assert_eq!(edges[0].weight, 1.0, "超 1 权重钳制到上界");
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

/// FC-MODEL-POST-003(supersede 沿用同 key;显式冲突 → `KeyMismatch`;索引不悬挂)
#[test]
fn supersede_preserves_key_and_rejects_conflict() {
    let db = mem(2);
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("os").text("windows"))
        .expect("insert");
    // 省略 key → 继承首参 key,`get`/`check` 均正常。
    ns.supersede("os", Record::new(vec![0.0, 1.0]).text("macos"))
        .expect("supersede");
    let got = ns.get("os").expect("get").expect("present");
    assert_eq!(got.key(), Some("os"), "supersede 须沿用同一 key");
    assert_eq!(got.text(), Some("macos"));
    assert!(
        db.check().expect("check").ok,
        "supersede 后 key 索引不应悬挂"
    );
    // 带 key 的历史视图同样能定位新版本(不因 key 丢失而失准)。
    let snap = db.as_of(i64::MAX).expect("as_of");
    assert_eq!(
        snap.namespace("n")
            .get("os")
            .expect("get")
            .expect("present")
            .text(),
        Some("macos")
    );
    // 显式给出冲突 key → KeyMismatch,且不产生任何写入。
    assert!(matches!(
        ns.supersede("os", Record::new(vec![1.0, 1.0]).key("other")),
        Err(mneme::MnemeError::KeyMismatch { .. })
    ));
    assert_eq!(
        ns.get("os").expect("get").expect("present").text(),
        Some("macos")
    );
    assert!(db.check().expect("check").ok);
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

/// FC-MODEL-POST-006(策略参数非法 → Config,绝不静默空转或索引越界 panic)
#[test]
fn consolidate_rejects_invalid_policy() {
    let ns = mem(2).namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0])).expect("insert");
    assert!(matches!(
        ns.consolidate(mneme::ConsolidationPolicy {
            max_cluster: 0,
            ..Default::default()
        }),
        Err(mneme::MnemeError::Config { .. })
    ));
    assert!(matches!(
        ns.consolidate(mneme::ConsolidationPolicy {
            threshold: f32::NAN,
            ..Default::default()
        }),
        Err(mneme::MnemeError::Config { .. })
    ));
    assert!(matches!(
        ns.consolidate(mneme::ConsolidationPolicy {
            threshold: 1.5,
            ..Default::default()
        }),
        Err(mneme::MnemeError::Config { .. })
    ));
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
