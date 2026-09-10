//! L2 持久层契约验收测试。
//!
//! 覆盖不变量 I1、I15、I16、I19、I20 与对应 `FC-PERSIST-*` 契约(见
//! `docs/spec/contracts.md` §2)。文件头引用的 `FC-*` 编号必须与契约矩阵中引用
//! 本文件的条目双向相等,由 `tests/contract_traceability.rs` 机械校验。
//!
//! 覆盖的契约:`FC-PERSIST-INV-001`、`FC-PERSIST-INV-019`、`FC-PERSIST-INV-020`、
//! `FC-PERSIST-POST-001`、`FC-PERSIST-POST-003`、`FC-PERSIST-POST-004`、
//! `FC-PERSIST-ERR-003`、`FC-PERSIST-ERR-004`。
//!
//! 片级编解码的损坏检出与版本拒绝见各 `src/persist/*.rs` 单元测试。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use mneme::{Builder, FsyncHook, IoAction, Mneme, Record, UpdatePatch};

/// 在临时目录新建持久库。
fn build(dir: &std::path::Path, dimension: u32) -> Mneme {
    Builder::default()
        .dimension(dimension)
        .path(dir)
        .build()
        .expect("build")
}

/// **FC-PERSIST-INV-001 / FC-PERSIST-POST-003(I1/I16)**:`close` 返回 `Ok` 后,
/// 全部已确认写入在重开后可读。
#[test]
fn reopen_after_close_recovers_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 4);
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0, 0.0, 0.0]).key("a"))
            .expect("insert a");
        ns.insert(Record::new(vec![0.0, 1.0, 0.0, 0.0]).key("b"))
            .expect("insert b");
        db.close().expect("close");
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert!(ns.get("a").expect("get a").is_some());
    assert!(ns.get("b").expect("get b").is_some());
    assert_eq!(db.list_namespaces().expect("list"), vec!["demo"]);
    db.close().expect("close");
}

/// **FC-PERSIST-INV-001(I1)**:未调用 `close` 直接释放(模拟崩溃),WAL 回放后
/// 已确认写入仍完整可见。
#[test]
fn reopen_after_drop_recovers_from_wal() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 3);
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 2.0, 3.0]).key("x"))
            .expect("insert");
    } // 不 close,直接 drop

    let db = Mneme::open(dir.path()).expect("reopen");
    assert!(db.namespace("demo").get("x").expect("get").is_some());
    db.close().expect("close");
}

/// **FC-PERSIST-POST-001(I15)**:`insert_batch` 整批持久,重开后可见记录数为整批。
#[test]
fn batch_insert_is_atomic_across_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        let outcomes = db
            .namespace("demo")
            .insert_batch(vec![
                Record::new(vec![1.0, 0.0]).key("a"),
                Record::new(vec![0.0, 1.0]).key("b"),
            ])
            .expect("insert_batch");
        assert_eq!(outcomes.len(), 2);
        db.close().expect("close");
    }
    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert!(ns.get("a").expect("get a").is_some());
    assert!(ns.get("b").expect("get b").is_some());
    db.close().expect("close");
}

/// **FC-PERSIST-INV-019(I19)**:`delete` 经 `flush` 物化进段后仍不复活。
#[test]
fn delete_survives_flush_and_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
        assert!(ns.delete("a").expect("delete a"));
        db.flush().expect("flush");
        db.close().expect("close");
    }
    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert!(ns.get("a").expect("get a").is_none());
    assert!(ns.get("b").expect("get b").is_some());
    db.close().expect("close");
}

/// **FC-PERSIST-INV-019(I19)**:`delete` 仅存在于 WAL(未 flush)时崩溃,
/// 回放后删除依然生效、墓碑不复活。
#[test]
fn crash_after_delete_does_not_resurrect() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        assert!(ns.delete("a").expect("delete a"));
    } // drop:仅 WAL 含 DeleteRow 帧

    let db = Mneme::open(dir.path()).expect("reopen");
    assert!(db.namespace("demo").get("a").expect("get a").is_none());
    db.close().expect("close");
}

/// **FC-PERSIST-INV-019(I19)**:`update` 重开后仍生效(新版本可读)。
#[test]
fn update_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        ns.update(
            "a",
            UpdatePatch {
                importance: Some(0.9),
                ..UpdatePatch::default()
            },
        )
        .expect("update");
        db.close().expect("close");
    }
    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    let rec = ns.get("a").expect("get a").expect("visible");
    assert_eq!(rec.importance(), 0.9);
    db.close().expect("close");
}

/// **FC-PERSIST-INV-020(I20)**:命名空间路径与 `RowId` 跨重启稳定。
#[test]
fn namespace_and_rowid_survive_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first_rowid = {
        let db = build(dir.path(), 2);
        let ns = db.namespace("proj/session");
        let rowid = match ns
            .insert(Record::new(vec![1.0, 0.0]).key("k"))
            .expect("insert")
        {
            mneme::InsertOutcome::Inserted(id) => id,
            other => panic!("unexpected: {other:?}"),
        };
        db.close().expect("close");
        rowid
    };

    let db = Mneme::open(dir.path()).expect("reopen");
    assert!(
        db.list_namespaces()
            .expect("list")
            .contains(&"proj/session".to_string())
    );
    let ns = db.namespace("proj/session");
    // upsert 同 key 复用既有 RowId,证明注册表与 next_rowid 已恢复。
    let second = match ns
        .insert(Record::new(vec![0.0, 1.0]).key("k"))
        .expect("upsert")
    {
        mneme::InsertOutcome::Inserted(id) => id,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(first_rowid, second);
    db.close().expect("close");
}

/// 只读模式:写操作返回结构化 `Unsupported`,绝不静默。
#[test]
fn read_only_rejects_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("seed");
        db.close().expect("close");
    }
    let db = Builder::default()
        .read_only(true)
        .path(dir.path())
        .build()
        .expect("read only open");
    assert!(db.namespace("demo").get("a").expect("get").is_some());
    assert!(matches!(
        db.namespace("demo").insert(Record::new(vec![0.0, 1.0])),
        Err(mneme::MnemeError::Unsupported { .. })
    ));
}

/// **FC-PERSIST-INV-001(I1)**:注入 WAL 写失败,失败写入整体回滚且不入 WAL;
/// 崩溃后恢复的可见状态 = 已确认操作前缀。
#[test]
fn injected_wal_failure_keeps_confirmed_prefix() {
    /// 在第 `fail_on` 次 WAL 写(0 基)前返回错误的钩子。
    struct FailAt {
        count: AtomicUsize,
        fail_on: usize,
    }
    impl FsyncHook for FailAt {
        fn before(&self, action: IoAction<'_>) -> std::io::Result<()> {
            if let IoAction::Write { file, .. } = action
                && file.starts_with("wal/")
            {
                let n = self.count.fetch_add(1, Ordering::SeqCst);
                if n == self.fail_on {
                    return Err(std::io::Error::other("injected WAL write failure"));
                }
            }
            Ok(())
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    // WAL 写序列:头(0)、首插批 [BatchBegin, NsRegister, Insert, BatchCommit](1..4)、
    // 第二次 insert 的 Insert 帧(5)。令索引 5 失败,首次插入保持已确认。
    let hook = Arc::new(FailAt {
        count: AtomicUsize::new(0),
        fail_on: 5,
    });
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .fsync_hook(hook)
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("first insert");
        assert!(ns.insert(Record::new(vec![0.0, 1.0]).key("b")).is_err());
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert!(ns.get("a").expect("get a").is_some());
    assert!(ns.get("b").expect("get b").is_none());
    db.close().expect("close");
}

/// **FC-PERSIST-POST-004**:`backup_to` 产出一致快照,可独立 `open`。
#[test]
fn backup_is_independently_openable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backup = tempfile::tempdir().expect("backup dir");
    {
        let db = build(dir.path(), 2);
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
        assert!(ns.delete("b").expect("delete b"));
        let report = db.backup_to(backup.path()).expect("backup");
        assert!(report.files >= 1);
        assert!(report.bytes > 0);
        db.close().expect("close");
    }

    let db = Mneme::open(backup.path()).expect("open backup");
    let ns = db.namespace("demo");
    assert!(ns.get("a").expect("get a").is_some());
    assert!(ns.get("b").expect("get b").is_none());
    db.close().expect("close");
}

/// 打开维度与库维度不符 → `DimensionMismatch`,拒绝打开。
#[test]
fn dimension_mismatch_rejected_on_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 4);
        db.close().expect("close");
    }
    assert!(matches!(
        Builder::default().dimension(8).path(dir.path()).build(),
        Err(mneme::MnemeError::DimensionMismatch { .. })
    ));
}
