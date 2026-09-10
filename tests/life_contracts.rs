//! L1 遗忘策略 / 库生命周期 / 错误分类契约验收测试。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-LIFE-INV-023、FC-LIFE-POST-001/002
//! * FC-MEM-ERR-001/002、FC-MEM-STA-001
//! * FC-GLOBAL-ERR-001/002、FC-GLOBAL-PRE-004
//!   (冒烟:任何公开 API 路径不 panic;错误分类变体语义互不混淆;策略参数非法 → Config)

use mneme::{Diversity, Expr, Mneme, Record, Retention};

mod common;

use common::{inserted, mem};

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

/// FC-MEM-ERR-002(延后能力返回结构化错误;`Fusion` 单独设置即拒绝)
#[test]
fn deferred_features_return_structured_errors() {
    let db = mem(2);
    let ns = db.namespace("n");
    assert!(matches!(
        ns.search().text("x").execute(),
        Err(mneme::MnemeError::Unsupported { .. })
    ));
    assert!(matches!(
        ns.search()
            .vector(&[1.0, 0.0])
            .fusion(mneme::Fusion::default())
            .execute(),
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

/// FC-GLOBAL-ERR-002 / FC-LIFE-POST-002 / FC-GLOBAL-PRE-004
/// (§0.2 错误分类矩阵:变体语义互不混淆;策略参数非有限值/越界 → Config)
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
    // 策略参数含非有限值 → Config(FC-GLOBAL-PRE-004 / FC-LIFE-POST-002)
    assert!(matches!(
        Mneme::builder()
            .dimension(2)
            .dedup_threshold(f32::NAN)
            .build(),
        Err(mneme::MnemeError::Config { .. })
    ));
    // 越界 [0,1] → Config(FC-GLOBAL-PRE-004)
    assert!(matches!(
        Mneme::builder().dimension(2).dedup_threshold(1.5).build(),
        Err(mneme::MnemeError::Config { .. })
    ));
    assert!(matches!(
        ns.retain(Retention::new().min_importance(f32::NAN)),
        Err(mneme::MnemeError::Config { .. })
    ));
}
