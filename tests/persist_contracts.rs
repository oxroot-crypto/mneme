//! L2 持久层契约验收测试。
//!
//! 覆盖不变量 I1–I4(持久化 / 损坏检测 / 段集合 / WAL 有界)、I11(备份可独立
//! 打开)、I15(批量原子)、I16(关闭持久)、I18(版本拒绝)、I19–I20(覆盖持久 /
//! 注册恢复)与对应 `FC-PERSIST-*` 契约(见
//! `docs/spec/contracts.md` §2)。文件头引用的 `FC-*` 编号必须与契约矩阵中引用
//! 本文件的条目双向相等,由 `tests/contract_traceability.rs` 机械校验。
//!
//! 覆盖的契约:`FC-PERSIST-INV-001`、`FC-PERSIST-INV-002`、`FC-PERSIST-INV-003`、
//! `FC-PERSIST-INV-004`、`FC-PERSIST-INV-005`、`FC-PERSIST-INV-006`、
//! `FC-PERSIST-INV-019`、`FC-PERSIST-INV-020`、`FC-PERSIST-INV-021`、
//! `FC-PERSIST-POST-001`、`FC-PERSIST-POST-002`、`FC-PERSIST-POST-003`、
//! `FC-PERSIST-POST-004`、`FC-PERSIST-POST-005`、`FC-PERSIST-POST-006`、
//! `FC-PERSIST-STA-001`、`FC-PERSIST-STA-002`、`FC-PERSIST-STA-003`、`FC-PERSIST-STA-004`、
//! `FC-PERSIST-ERR-002`、`FC-PERSIST-ERR-003`、`FC-PERSIST-ERR-004`、
//! `FC-PERSIST-ERR-005`、`FC-PERSIST-ERR-006`、`FC-PERSIST-ERR-007`、`FC-PERSIST-CPLX-001`、`FC-PERSIST-CPLX-007`、
//! `FC-PERSIST-CPLX-008`、`FC-PERSIST-CPLX-009`、`FC-PERSIST-CPLX-010`、`FC-INDEX-ERR-002`、
//! `FC-LIFE-INV-011`(备份独立打开 + 校验)、`FC-LIFE-CPLX-005`(backup/check 哨兵)。
//!
//! 片级编解码的损坏检出与版本拒绝见各 `src/persist/*.rs` 单元测试。

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use mneme::{
    Builder, CompactionPolicy, FsyncHook, IoAction, Metric, Mneme, Record, Tuning, UpdatePatch,
};

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
    let ns = db.namespace("demo");
    let record = ns.get("x").expect("get").expect("已确认写入必须完整可见");
    assert_eq!(record.key(), Some("x"));
    assert_eq!(record.vector(), [1.0, 2.0, 3.0]);
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
        assert!(matches!(
            ns.insert(Record::new(vec![0.0, 1.0]).key("b")),
            Err(mneme::MnemeError::Io(_))
        ));
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert!(ns.get("a").expect("get a").is_some());
    assert!(ns.get("b").expect("get b").is_none());
    db.close().expect("close");
}

/// **FC-PERSIST-INV-001(I1)**:写事务的 append/sync 失败时截断半写帧——
/// 返回 `Err` 的写绝不持久,且不遮挡后续已确认写。
#[test]
fn injected_fsync_failure_rolls_back_frames() {
    /// 在第 `fail_on` 次 WAL fsync(0 基)前返回错误的钩子。
    struct FailFsyncAt {
        count: AtomicUsize,
        fail_on: usize,
    }
    impl FsyncHook for FailFsyncAt {
        fn before(&self, action: IoAction<'_>) -> std::io::Result<()> {
            if let IoAction::Fsync { file } = action
                && file.starts_with("wal/")
            {
                let n = self.count.fetch_add(1, Ordering::SeqCst);
                if n == self.fail_on {
                    return Err(std::io::Error::other("injected WAL fsync failure"));
                }
            }
            Ok(())
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let hook = Arc::new(FailFsyncAt {
        count: AtomicUsize::new(0),
        fail_on: 0,
    });
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .fsync_hook(Arc::clone(&hook) as Arc<dyn FsyncHook>)
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        assert!(
            matches!(
                ns.insert(Record::new(vec![1.0, 0.0]).key("a")),
                Err(mneme::MnemeError::Io(_))
            ),
            "fsync 失败必须以 Io 变体使写事务报错"
        );
        // fsync 仅失败一次;后续已确认写必须成功。
        ns.insert(Record::new(vec![0.0, 1.0]).key("b"))
            .expect("second insert");
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert!(
        ns.get("a").expect("get a").is_none(),
        "失败的写绝不持久(半写帧已被截断)"
    );
    assert!(
        ns.get("b").expect("get b").is_some(),
        "后续已确认写不得被残帧遮挡"
    );
    db.close().expect("close");
}

/// **FC-PERSIST-POST-005**:`relate`/`unrelate` 仅存于 WAL 时崩溃,回放后仍生效。
#[test]
fn relate_and_unrelate_survive_crash() {
    use mneme::{InsertOutcome, RelationKind};

    let dir = tempfile::tempdir().expect("tempdir");
    let (a, b) = {
        let db = build(dir.path(), 2);
        let ns = db.namespace("demo");
        let a = match ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a") {
            InsertOutcome::Inserted(id) => id,
            other => panic!("unexpected: {other:?}"),
        };
        let b = match ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b") {
            InsertOutcome::Inserted(id) => id,
            other => panic!("unexpected: {other:?}"),
        };
        ns.relate(a, b, RelationKind::SUPPORTS, 0.7)
            .expect("relate");
        ns.relate(a, b, RelationKind::RELATED, 0.5)
            .expect("relate2");
        assert!(ns.unrelate(a, b, RelationKind::RELATED).expect("unrelate"));
        (a, b)
    };

    // 第一个进程 drop(未 close):Relate/Unrelate 仅在 WAL 中。
    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    let edges = ns.neighbors(a, &[]).expect("neighbors");
    assert_eq!(edges.len(), 1, "只应保留 SUPPORTS 边");
    assert_eq!(edges[0].kind, RelationKind::SUPPORTS);
    assert_eq!(b.get(), edges[0].to.get());
    assert!(
        ns.neighbors(a, &[RelationKind::RELATED])
            .expect("neighbors")
            .is_empty(),
        "被 unrelate 的边不得复活"
    );
    db.close().expect("close");
}

/// **FC-PERSIST-POST-005**:`touch` 的 importance 强化与访问统计经 WAL 在崩溃后保留。
#[test]
fn touch_boost_survives_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        assert!(ns.touch("a", Some(0.3)).expect("touch"));
    }
    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    let rec = ns.get("a").expect("get").expect("visible");
    assert!((rec.importance() - 0.8).abs() < 1e-6);
    // `TouchRow` 帧的访问统计必须回放恢复(仅 importance 由版本 `Insert` 承载)。
    let touched = ns
        .count(Some(mneme::Expr::field("access_count").eq(1)))
        .expect("count access_count");
    assert_eq!(touched, 1, "TouchRow 回放必须恢复访问统计");
    db.close().expect("close");
}

/// **FC-PERSIST-ERR-002(I18)**:段格式版本与当前定义不一致(无论高低)→
/// `UnsupportedVersion`,即使默认非 fail-fast 也拒绝打开(绝不降级为跳过)。
#[test]
fn segment_version_mismatch_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    let vsec = dir.path().join("segments").join("seg_000000.vsec");
    let original = std::fs::read(&vsec).expect("read");
    // 版本在 head 校验之前判定,故无需重算头部 CRC;高/低版本都必须拒绝。
    for version in [0x0100_u16, 0x0003] {
        let mut bytes = original.clone();
        bytes[4..6].copy_from_slice(&version.to_le_bytes());
        std::fs::write(&vsec, &bytes).expect("write");
        assert!(
            matches!(
                Mneme::open(dir.path()),
                Err(mneme::MnemeError::UnsupportedVersion { .. })
            ),
            "版本 {version:#06x} 必须拒绝"
        );
    }
    std::fs::write(&vsec, &original).expect("restore");
}

/// **FC-PERSIST-ERR-003**:只读打开不创建/改写 WAL 文件,且可读既有数据。
#[test]
fn read_only_open_does_not_create_wal() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    let wal = dir.path().join("wal").join("wal_000001.log");
    std::fs::remove_file(&wal).expect("remove wal");

    let db = Builder::default()
        .read_only(true)
        .path(dir.path())
        .build()
        .expect("read only open");
    assert!(
        db.namespace("demo").get("a").expect("get").is_some(),
        "段数据可读"
    );
    assert!(!wal.exists(), "只读打开不得创建 WAL");
    db.close().expect("close");
}

/// **FC-PERSIST-INV-002(I2)**:`check()` 逐段校验,损坏段被报告为 `Corrupted`。
#[test]
fn check_detects_corrupt_segment() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 4);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 2.0, 3.0, 4.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    let vsec = dir.path().join("segments").join("seg_000000.vsec");
    let mut bytes = std::fs::read(&vsec).expect("read");
    bytes[70] ^= 0xFF;
    std::fs::write(&vsec, &bytes).expect("write");

    let db = Mneme::open(dir.path()).expect("open");
    let report = db.check().expect("check");
    assert!(!report.ok, "损坏段必须使 check 失败");
    assert_eq!(report.corrupted.len(), 1);
    assert_eq!(report.corrupted[0].get(), 0);
    db.close().expect("close");
}

/// **FC-PERSIST-ERR-005**:`current` 存在但无合法 MANIFEST → `Corrupted`,拒绝覆盖。
#[test]
fn corrupt_current_without_valid_manifest_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    std::fs::write(dir.path().join("current"), b"not-a-number").expect("corrupt current");
    // 破坏唯一的 MANIFEST,使扫描也找不到合法版本。
    let manifest_file = dir.path().join("MANIFEST.000001");
    let mut bytes = std::fs::read(&manifest_file).expect("read manifest");
    bytes[12] ^= 0xFF;
    std::fs::write(&manifest_file, &bytes).expect("corrupt manifest");

    assert!(matches!(
        Mneme::open(dir.path()),
        Err(mneme::MnemeError::Corrupted { .. })
    ));
}

/// **FC-PERSIST-ERR-005**:回退扫描按目录中的实际文件名读取 MANIFEST——
/// 非规范命名(如缺前导零的 `MANIFEST.1`)仍可回退打开,不因重建规范名而读空。
#[test]
fn manifest_fallback_accepts_noncanonical_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    std::fs::rename(
        dir.path().join("MANIFEST.000001"),
        dir.path().join("MANIFEST.1"),
    )
    .expect("rename manifest");

    let db = Mneme::open(dir.path()).expect("非规范命名 MANIFEST 应可回退打开");
    let ns = db.namespace("demo");
    let record = ns.get("a").expect("get").expect("记录存在");
    assert_eq!(record.vector(), [1.0, 0.0]);
    db.close().expect("close");
}

/// **FC-PERSIST-ERR-005**:存在段文件却无 MANIFEST → `Corrupted`,拒绝当作新库覆盖。
#[test]
fn segments_without_manifest_are_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("segments")).expect("mkdir");
    std::fs::write(
        dir.path().join("segments").join("seg_000000.vsec"),
        b"VSC1 garbage",
    )
    .expect("write orphan segment");

    assert!(matches!(
        Builder::default().dimension(2).path(dir.path()).build(),
        Err(mneme::MnemeError::Corrupted { .. })
    ));
}

/// **FC-PERSIST-STA-002**:MANIFEST 未引用的段孤儿在可写打开时清理。
#[test]
fn unreferenced_segment_cleaned_on_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    let stray = dir.path().join("segments").join("seg_000042.vsec");
    std::fs::write(&stray, b"VSC1 orphan").expect("write stray");
    let _db = Mneme::open(dir.path()).expect("reopen");
    assert!(!stray.exists(), "未引用段必须被清理");
}

/// **FC-PERSIST-POST-004(I11)**:**FC-LIFE-INV-011** 的落地锚点:备份产物可独立
/// `open` + `check`;`backup_to` 产出一致快照。
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
    // 备份必须包含 hidx 且可独立载入(FC-PERSIST-POST-004 + L3 索引)。
    assert!(
        db.stats().expect("stats").segments[0].index_nodes > 0,
        "备份应包含并可载入 hidx"
    );
    // FC-LIFE-INV-011:备份目录不仅可独立 open,还必须通过 check。
    assert!(db.check().expect("check").ok, "备份目录 check 必须通过");
    db.close().expect("close");
}

/// **FC-PERSIST-INV-004(I4)**:WAL 达到硬上限(`wal_bytes × 12`)自动触发增量段
/// flush,WAL 有界、数据不丢。
#[test]
fn wal_capacity_triggers_incremental_flush() {
    let dir = tempfile::tempdir().expect("tempdir");
    let compaction = mneme::CompactionPolicy {
        wal_bytes: 256,
        ..Default::default()
    };
    {
        let db = Builder::default()
            .dimension(8)
            .compaction(compaction)
            .path(dir.path())
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        for row in 0..200_u32 {
            let vector: Vec<f32> = (0..8).map(|col| (row + col) as f32).collect();
            ns.insert(Record::new(vector).key(format!("k{row}")))
                .expect("insert");
        }
        let stats = db.stats().expect("stats");
        assert!(
            stats.wal_bytes < 12 * 256,
            "WAL 必须被自动 flush 截断(硬上限内):{} 字节",
            stats.wal_bytes
        );
        assert!(!stats.segments.is_empty(), "自动 flush 必须产生段");
    } // 释放库与命名空间句柄,释放独占锁

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert!(ns.get("k0").expect("k0").is_some());
    assert!(ns.get("k199").expect("k199").is_some());
    db.close().expect("close");
}

/// **FC-PERSIST-INV-004(I4)**:`wal_bytes` 为软阈值——达到后若未落盘行数不足
/// 「并行度 × 块行数」,继续累积(避免大维度下每次只物化一个块单核串行);
/// WAL 达硬上限(8×软阈值)仍必须强制 flush,保证有界。
#[test]
fn wal_soft_threshold_waits_for_parallel_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let compaction = mneme::CompactionPolicy {
        wal_bytes: 1_024,
        ..Default::default()
    };
    {
        let db = Builder::default()
            .dimension(8)
            .compaction(compaction)
            .path(dir.path())
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        // 40 行(约 3 KiB)越过软阈值但远未达硬上限(8 KiB),且不足并行行数。
        for row in 0..40_u32 {
            let vector: Vec<f32> = (0..8).map(|col| (row + col) as f32).collect();
            ns.insert(Record::new(vector).key(format!("w{row}")))
                .expect("insert");
        }
        let stats = db.stats().expect("stats");
        assert!(
            stats.wal_bytes > 1_024,
            "测试前提:WAL 已越过软阈值({} 字节)",
            stats.wal_bytes
        );
        assert!(
            stats.wal_bytes < 12 * 1_024,
            "测试前提:尚未越过硬上限({} 字节)",
            stats.wal_bytes
        );
        assert!(
            stats.segments.is_empty(),
            "未达硬上限且行数不足时不得提前 flush(否则大维度退化为单块串行)"
        );
        // 继续写入越过硬上限(8 KiB):必须强制 flush。
        for row in 40..200_u32 {
            let vector: Vec<f32> = (0..8).map(|col| (row + col) as f32).collect();
            ns.insert(Record::new(vector).key(format!("w{row}")))
                .expect("insert");
        }
        let stats = db.stats().expect("stats");
        assert!(!stats.segments.is_empty(), "越过硬上限必须自动 flush");
        assert!(
            stats.wal_bytes < 12 * 1_024,
            "Checkpoint 后 WAL 回到硬上限内"
        );
    }
}

/// **FC-PERSIST-INV-003(I3) / FC-PERSIST-POST-002**:flush 先物化覆盖条目再重置
/// WAL(Checkpoint),活跃段 = MANIFEST 所列集合。
#[test]
fn flush_checkpoints_after_materialize() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
        assert!(ns.delete("a").expect("delete a"));
        db.flush().expect("flush");

        let stats = db.stats().expect("stats");
        assert_eq!(stats.segments.len(), 1, "活跃段 = MANIFEST 所列集合");
        assert!(stats.wal_bytes <= 64, "Checkpoint 后 WAL 只剩文件头");
    } // 释放句柄与独占锁

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert!(ns.get("a").expect("a").is_none(), "删除已在 flush 时物化");
    assert!(ns.get("b").expect("b").is_some());
    db.close().expect("close");
}

/// **FC-PERSIST-STA-002**:崩溃残留的 `.tmp`(`Building` 孤儿)在恢复时清理。
#[test]
fn orphan_tmp_cleaned_on_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    let orphan = dir.path().join("segments").join("seg_999999.vsec.tmp");
    std::fs::write(&orphan, b"half-written").expect("write orphan");
    let _db = Mneme::open(dir.path()).expect("reopen");
    assert!(!orphan.exists(), "孤儿 .tmp 必须被清理");
}

/// **FC-PERSIST-STA-001**:已提交段内容不可变(write-once),重写产生新段。
#[test]
fn committed_segment_is_write_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = build(dir.path(), 2);
    db.namespace("demo")
        .insert(Record::new(vec![1.0, 0.0]).key("a"))
        .expect("a");
    db.flush().expect("flush");
    let first = dir.path().join("segments").join("seg_000000.vsec");
    let before = std::fs::read(&first).expect("read seg");

    db.namespace("demo")
        .insert(Record::new(vec![0.0, 1.0]).key("b"))
        .expect("b");
    let after = std::fs::read(&first).expect("read seg again");
    assert_eq!(before, after, "已提交段在下次 flush 前不得被改写");

    db.flush().expect("second flush");
    assert!(
        dir.path().join("segments").join("seg_000001.vsec").exists(),
        "重写产生新段 id"
    );
    db.close().expect("close");
}

/// **FC-PERSIST-INV-002(I2)**:`verify_on_open` + fail-fast 在 payload 损坏时拒绝启动。
#[test]
fn verify_on_open_detects_payload_corruption() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 4);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 2.0, 3.0, 4.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    let vsec = dir.path().join("segments").join("seg_000000.vsec");
    let mut bytes = std::fs::read(&vsec).expect("read");
    // 翻转数据区一个字节(头部 64B 之后),头部 CRC 仍合法、payload CRC 被破坏。
    bytes[70] ^= 0xFF;
    std::fs::write(&vsec, &bytes).expect("write");

    assert!(matches!(
        Builder::default()
            .verify_on_open(true)
            .fail_fast_on_corruption(true)
            .path(dir.path())
            .build(),
        Err(mneme::MnemeError::Corrupted { .. })
    ));
}

/// **FC-PERSIST-CPLX-001**:整批写入只 fsync 一次(组提交)。
#[test]
fn batch_insert_uses_single_fsync() {
    struct CountFsync {
        fsyncs: AtomicUsize,
    }
    impl FsyncHook for CountFsync {
        fn before(&self, action: IoAction<'_>) -> std::io::Result<()> {
            if matches!(action, IoAction::Fsync { .. }) {
                self.fsyncs.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let hook = Arc::new(CountFsync {
        fsyncs: AtomicUsize::new(0),
    });
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .fsync_hook(Arc::clone(&hook) as Arc<dyn FsyncHook>)
        .build()
        .expect("build");
    let before = hook.fsyncs.load(Ordering::SeqCst);
    db.namespace("demo")
        .insert_batch(vec![
            Record::new(vec![1.0, 0.0]).key("a"),
            Record::new(vec![0.0, 1.0]).key("b"),
            Record::new(vec![0.5, 0.5]).key("c"),
        ])
        .expect("insert_batch");
    let after = hook.fsyncs.load(Ordering::SeqCst);
    assert_eq!(after - before, 1, "一个写事务只 fsync 一次");
    db.close().expect("close");
}

/// **FC-PERSIST-CPLX-010(哨兵)**:版本链跨重启保留(`as_of` 数据源不丢)。
#[test]
fn version_chain_survives_reopen() {
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
        assert!(db.stats().expect("stats").history.retained_versions >= 2);
    }
    let db = Mneme::open(dir.path()).expect("reopen");
    assert!(
        db.stats().expect("stats").history.retained_versions >= 2,
        "版本链跨重启保留"
    );
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

/// **FC-PERSIST-INV-005(I1)**:WAL 撕裂尾部在可写重开时被物理截断,其后追加的
/// 已确认写入不会因残尾被永久屏蔽而丢失。
#[test]
fn torn_wal_tail_truncated_on_reopen() {
    use std::io::Write as _;

    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
    } // drop:WAL 仅含已 fsync 的帧

    // 模拟撕裂帧:在 WAL 末尾追加不足一帧的垃圾字节。
    {
        let wal = dir.path().join("wal").join("wal_000001.log");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&wal)
            .expect("open wal");
        file.write_all(&[0xFF, 0x00, 0x01, 0x02])
            .expect("append tail");
        file.sync_all().expect("sync");
    }

    {
        let db = Mneme::open(dir.path()).expect("reopen");
        let ns = db.namespace("demo");
        assert!(ns.get("a").expect("get a").is_some(), "有效前缀必须恢复");
        ns.insert(Record::new(vec![0.0, 1.0]).key("b"))
            .expect("insert b");
    } // drop:截断后追加的 b 必须保留

    let db = Mneme::open(dir.path()).expect("reopen2");
    let ns = db.namespace("demo");
    assert!(ns.get("a").expect("get a").is_some());
    assert!(
        ns.get("b").expect("get b").is_some(),
        "撕裂尾部之后的已确认写入不得丢失"
    );
    db.close().expect("close");
}

/// **FC-PERSIST-INV-005(I1)**:WAL 短于文件头(Checkpoint 重置中途崩溃)在可写重开时
/// 被重建,绝不因半截头拒绝打开。
#[test]
fn short_wal_header_is_recreated_on_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.close().expect("close"); // flush → WAL 重置为仅 32B 文件头
    }
    let wal = dir.path().join("wal").join("wal_000001.log");
    std::fs::write(&wal, b"WAL1").expect("truncate wal"); // 5B < 32B

    let db = Mneme::open(dir.path()).expect("reopen");
    assert!(
        db.namespace("demo").get("a").expect("get a").is_some(),
        "段数据不受半截 WAL 头影响"
    );
    db.close().expect("close");
}

/// **FC-PERSIST-ERR-007(I1/I15)**:崩溃在 `BatchCommit` 前(批未闭合)时,未闭合批的
/// 字节在重开时被截断,绝不残留并吞掉后续单操作事务(删除不复活)。
#[test]
fn unclosed_batch_tail_does_not_swallow_later_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        let ns = db.namespace("demo");
        ns.insert_batch(vec![
            Record::new(vec![1.0, 0.0]).key("a"),
            Record::new(vec![0.0, 1.0]).key("b"),
        ])
        .expect("batch1");
        ns.insert_batch(vec![
            Record::new(vec![0.5, 0.5]).key("c"),
            Record::new(vec![0.2, 0.8]).key("d"),
        ])
        .expect("batch2");
    }
    // 撕裂最后一个 `BatchCommit` 帧(截掉其尾部),形成未闭合批。
    let wal = dir.path().join("wal").join("wal_000001.log");
    let len = std::fs::metadata(&wal).expect("meta").len();
    {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&wal)
            .expect("open wal");
        file.set_len(len - 5).expect("truncate");
        file.sync_all().expect("sync");
    }

    {
        let db = Mneme::open(dir.path()).expect("reopen1");
        let ns = db.namespace("demo");
        assert!(ns.get("a").expect("a").is_some());
        assert!(ns.get("c").expect("c").is_none(), "未闭合批不得应用");
        // 单操作事务:delete 不产生 `BatchBegin`。
        assert!(ns.delete("a").expect("delete a"));
    } // drop

    let db = Mneme::open(dir.path()).expect("reopen2");
    assert!(
        db.namespace("demo").get("a").expect("get a").is_none(),
        "已确认删除不得被残留未闭合批吞掉而复活"
    );
    db.close().expect("close");
}

/// **FC-PERSIST-INV-006**:WAL 落盘成功即提交点;其后 flush 失败不回滚已提交写,
/// 也不产生「返回失败却重启可见」的矛盾。
#[test]
fn flush_failure_does_not_lose_committed_write() {
    struct FailSegment;
    impl FsyncHook for FailSegment {
        fn before(&self, action: IoAction<'_>) -> std::io::Result<()> {
            if let IoAction::Write { file, .. } = action
                && file.starts_with("segments/")
            {
                return Err(std::io::Error::other("injected segment write failure"));
            }
            Ok(())
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let compaction = mneme::CompactionPolicy {
        wal_bytes: 32,
        ..Default::default()
    };
    {
        let db = Builder::default()
            .dimension(2)
            .compaction(compaction)
            .path(dir.path())
            .fsync_hook(Arc::new(FailSegment))
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        // 插到越过硬上限(32 × 12 = 384 字节)触发自动 flush;段写入被注入失败,
        // 但已提交写必须照常成功(FC-PERSIST-INV-006)。
        for key in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            ns.insert(Record::new(vec![1.0, 0.0]).key(key))
                .expect("已提交写不得因后台 flush 失败而报错");
        }
        assert!(
            db.stats().expect("stats").wal_bytes > 32,
            "flush 失败可由 wal_bytes 超过阈值观测到"
        );
    } // drop

    let db = Mneme::open(dir.path()).expect("reopen");
    assert!(
        db.namespace("demo").get("a").expect("get a").is_some(),
        "WAL 重放恢复已提交写"
    );
    db.close().expect("close");
}

/// **FC-PERSIST-STA-003**:首次 flush 在段已写、MANIFEST 未提交时崩溃;
/// 重开以 WAL 为准恢复并清理未提交孤儿段,绝不误判为 `Corrupted`。
#[test]
fn first_flush_crash_recovers_from_wal() {
    struct FailManifest;
    impl FsyncHook for FailManifest {
        fn before(&self, action: IoAction<'_>) -> std::io::Result<()> {
            if let IoAction::Write { file, .. } = action
                && file.starts_with("MANIFEST.")
            {
                return Err(std::io::Error::other("injected manifest write failure"));
            }
            Ok(())
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .fsync_hook(Arc::new(FailManifest))
            .build()
            .expect("build");
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        assert!(
            db.flush().is_err(),
            "注入的 MANIFEST 写失败必须使 flush 报错"
        );
    }
    let orphan = dir.path().join("segments").join("seg_000000.vsec");
    assert!(orphan.exists(), "崩溃点应留下已写段文件");

    let db = Mneme::open(dir.path()).expect("以 WAL 为准重开");
    let ns = db.namespace("demo");
    assert!(ns.get("a").expect("get a").is_some(), "WAL 恢复记录");
    assert!(!orphan.exists(), "未提交孤儿段在可写打开时被清理");
    db.close().expect("close");
}

/// **FC-PERSIST-POST-006**:事务时间随 WAL 持久化,崩溃恢复后 `as_of` 历史正确
/// (删除前时点仍可见、删除后不可见)。
#[test]
fn as_of_history_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = common::FakeClock::default();
    {
        clock.set(1_000);
        let db = Builder::default()
            .dimension(2)
            .clock(Arc::new(clock.clone()))
            .path(dir.path())
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        clock.set(2_000);
        assert!(ns.delete("a").expect("delete"));
    } // drop:WAL 含 Insert(tx=1000) 与 DeleteRow(tx=2000)

    let db = Mneme::open(dir.path()).expect("reopen");
    let at_1500 = db.as_of(1_500).expect("as_of");
    assert!(
        at_1500.namespace("demo").get("a").expect("get").is_some(),
        "删除前的 as_of 时点仍应可见"
    );
    assert!(db.namespace("demo").get("a").expect("get").is_none());
    db.close().expect("close");
}

/// **FC-PERSIST-ERR-006(I2/I3)**:MANIFEST 引用的段文件缺失 → `Corrupted`,
/// 绝不静默少返回数据。
#[test]
fn referenced_segment_missing_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    std::fs::remove_file(dir.path().join("segments").join("seg_000000.msec")).expect("remove msec");
    assert!(matches!(
        Mneme::open(dir.path()),
        Err(mneme::MnemeError::Corrupted { .. })
    ));
}

/// **FC-PERSIST-ERR-006(I2/I3)**:MANIFEST 引用的段文件为空 → `Corrupted`。
#[test]
fn referenced_segment_empty_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    std::fs::write(dir.path().join("segments").join("seg_000000.vsec"), b"").expect("empty vsec");
    assert!(matches!(
        Mneme::open(dir.path()),
        Err(mneme::MnemeError::Corrupted { .. })
    ));
}

/// **FC-PERSIST-ERR-003**:只读打开绝不改动文件系统(不建目录、不清 trash)。
#[test]
fn read_only_open_does_not_mutate() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    let trash = dir.path().join("trash");
    std::fs::create_dir_all(&trash).expect("mkdir trash");
    let junk = trash.join("keep.me");
    std::fs::write(&junk, b"x").expect("write junk");

    let db = Builder::default()
        .read_only(true)
        .path(dir.path())
        .build()
        .expect("read only open");
    assert!(junk.exists(), "只读打开不得清理 trash");
    db.close().expect("close");

    // 只读打开不存在的库必须报错,而不是创建目录树。
    let missing = dir.path().join("does-not-exist");
    assert!(matches!(
        Builder::default()
            .read_only(true)
            .dimension(2)
            .path(&missing)
            .build(),
        Err(mneme::MnemeError::Config { .. })
    ));
    assert!(!missing.exists(), "只读打开不得创建目录");
}

/// **FC-INDEX-ERR-002(L3)**:MANIFEST 引用的 hidx 缺失时,fail-fast 打开直接拒绝;
/// 可写非 fail-fast 打开降级为暴力(`stats.index_nodes == 0`)且 `check()` 报告损坏。
#[test]
fn missing_hidx_degrades_or_rejects() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    let hidx = dir.path().join("segments").join("seg_000000.hidx");
    assert!(hidx.exists(), "flush 应写出 hidx");
    std::fs::remove_file(&hidx).expect("remove hidx");

    // fail-fast:拒绝打开(此时 MANIFEST 仍引用缺失的 hidx)。
    assert!(matches!(
        Builder::default()
            .fail_fast_on_corruption(true)
            .path(dir.path())
            .build(),
        Err(mneme::MnemeError::Corrupted { .. })
    ));

    // 非 fail-fast:降级暴力,库可读,check 报告损坏。
    let db = Builder::default()
        .path(dir.path())
        .build()
        .expect("degrade open");
    let stats = db.stats().expect("stats");
    assert_eq!(stats.segments[0].index_nodes, 0, "缺 hidx 应降级暴力");
    assert!(
        db.namespace("demo").get("a").expect("get").is_some(),
        "降级后库必须仍可读"
    );
    assert!(!db.check().expect("check").ok, "缺 hidx 应被 check 报告");
}

/// **FC-INDEX-ERR-002(L3)**:hidx 内容被翻转(CRC 不符)时行为与缺失一致。
#[test]
fn corrupt_hidx_degrades_or_rejects() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.close().expect("close");
    }
    let hidx = dir.path().join("segments").join("seg_000000.hidx");
    assert!(hidx.exists(), "flush 应写出 hidx");
    let mut bytes = std::fs::read(&hidx).expect("read hidx");
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    std::fs::write(&hidx, &bytes).expect("write hidx");

    assert!(matches!(
        Builder::default()
            .fail_fast_on_corruption(true)
            .path(dir.path())
            .build(),
        Err(mneme::MnemeError::Corrupted { .. })
    ));

    let db = Builder::default()
        .path(dir.path())
        .build()
        .expect("degrade open");
    assert_eq!(db.stats().expect("stats").segments[0].index_nodes, 0);
    assert!(
        db.namespace("demo").get("a").expect("get").is_some(),
        "降级后库必须仍可读"
    );
    assert!(!db.check().expect("check").ok, "坏 hidx 应被 check 报告");
}

/// **FC-PERSIST-INV-021**:重开经惰性段句柄读出的向量与检索排序,与写路径
/// 自有向量及客户端暴力点积参照逐位一致;二次访问命中解码缓存。
#[test]
fn lazy_reopen_matches_bruteforce() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dimension = 8_usize;
    let rows = 128_usize;
    let vectors: Vec<Vec<f32>> = (0..rows)
        .map(|row| {
            (0..dimension)
                .map(|col| ((row * 31 + col) % 97) as f32 / 97.0)
                .collect()
        })
        .collect();
    {
        let db = Builder::default()
            .dimension(dimension as u32)
            .metric(Metric::Dot)
            .path(dir.path())
            .build()
            .expect("build");
        let ns = db.namespace("lazy");
        for (row, vector) in vectors.iter().enumerate() {
            ns.insert(Record::new(vector.clone()).key(format!("k{row}")))
                .expect("insert");
        }
        db.flush().expect("flush");
        db.close().expect("close");
    }

    let db = Builder::default()
        .path(dir.path())
        .tuning(Tuning {
            brute_force_max_rows: 1,
            ..Tuning::default()
        })
        .build()
        .expect("reopen");
    let ns = db.namespace("lazy");
    for (row, expected) in vectors.iter().enumerate() {
        let record = ns.get(&format!("k{row}")).expect("get").expect("存在");
        assert_eq!(record.vector(), expected.as_slice(), "惰性解码必须逐位一致");
    }
    let query: Vec<f32> = (0..dimension).map(|col| (col + 1) as f32 / 8.0).collect();
    let dot = |vector: &[f32]| {
        vector
            .iter()
            .zip(&query)
            .map(|(left, right)| left * right)
            .sum::<f32>()
    };
    let mut expected_top: Vec<usize> = (0..rows).collect();
    expected_top.sort_by(|&left, &right| {
        dot(&vectors[right])
            .total_cmp(&dot(&vectors[left]))
            .then(left.cmp(&right))
    });
    let hits = ns
        .search()
        .vector(&query)
        .top_k(10)
        .ef(256)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 10);
    assert_eq!(
        hits[0].key.as_ref().expect("key").as_str(),
        format!("k{}", expected_top[0]),
        "惰性索引载入的首名必须与暴力点积一致"
    );
    // 同参数二次检索逐位一致(命中惰性解码缓存,结果不漂移)。
    let again = ns
        .search()
        .vector(&query)
        .top_k(10)
        .ef(256)
        .execute()
        .expect("search twice");
    let first: Vec<_> = hits.iter().map(|hit| hit.rowid).collect();
    let second: Vec<_> = again.iter().map(|hit| hit.rowid).collect();
    assert_eq!(first, second, "重复检索结果必须稳定");
    db.close().expect("close");
}

/// **FC-PERSIST-INV-021**:compaction 把旧段移入 `trash/` 后,快照句柄上钉住的
/// 惰性向量与索引仍可读,结果与合并前一致(句柄存活期内数据不失效)。
#[test]
fn lazy_handles_survive_compaction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .dimension(4)
        .path(dir.path())
        .compaction(CompactionPolicy {
            tier_ratio: 2,
            tier_count: 2,
            segment_rows: 4,
            ..CompactionPolicy::default()
        })
        .tuning(Tuning {
            brute_force_max_rows: 1,
            ..Tuning::default()
        })
        .build()
        .expect("build");
    let ns = db.namespace("lazy");
    let mut first_batch = Vec::new();
    for row in 0..16_u32 {
        let vector = vec![row as f32, 1.0, 0.0, 0.0];
        ns.insert(Record::new(vector.clone()).key(format!("k{row}")))
            .expect("insert");
        first_batch.push(vector);
    }
    db.flush().expect("flush");
    let snapshot = db.snapshot();
    for row in 16..32_u32 {
        ns.insert(Record::new(vec![row as f32, 1.0, 0.0, 0.0]).key(format!("k{row}")))
            .expect("insert");
    }
    db.flush().expect("flush");
    db.compact().expect("compact");

    let snap_ns = snapshot.namespace("lazy");
    for (row, vector) in first_batch.iter().enumerate() {
        let record = snap_ns.get(&format!("k{row}")).expect("get").expect("存在");
        assert_eq!(record.vector(), vector.as_slice(), "旧段句柄必须仍可读");
    }
    let hits = snap_ns
        .search()
        .vector(&[0.0, 1.0, 0.0, 0.0])
        .top_k(16)
        .ef(128)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 16, "快照视图必须仍返回全部旧记录");
    db.close().expect("close");
}

#[cfg(all(feature = "mmap", not(feature = "wasm")))]
use common::SegmentReadSpy;

/// **FC-PERSIST-INV-021 / FC-PERSIST-CPLX-007(mmap 构建)**:打开未加密段只做
/// 4 字节信封探测(`read_prefix`)与惰性视图(`open_bytes`),不得经 `read_file`
/// 整读段文件——否则 1M×1536 冷启动门槛与「向量区不物化」承诺失效。
#[cfg(all(feature = "mmap", not(feature = "wasm")))]
#[test]
fn segment_open_probes_prefix_without_full_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 4);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0, 0.0, 0.0]).key("a"))
            .expect("insert");
        db.flush().expect("flush");
        db.close().expect("close");
    }

    let spy = SegmentReadSpy::new(dir.path());
    let full_reads = std::sync::Arc::clone(&spy.full_reads);
    let db = Builder::default()
        .dimension(4)
        .path(dir.path())
        .storage(Arc::new(spy))
        .build()
        .expect("reopen with spy");
    drop(db);

    let reads = full_reads.lock().expect("lock");
    assert!(
        reads.is_empty(),
        "打开未加密段不得经 read_file 整读,实际调用: {reads:?}"
    );
}

/// **FC-PERSIST-STA-004**:大规模 flush 切块并行构建(块数 ≤ 并行度)时仍一次
/// 提交多段;段集与数据完整、`check` 通过、重开一致。小规模保持单段。
///
/// `#[ignore]`:70k 行建索引在 debug 下约 3 分钟(切分逻辑由
/// `src/persist/store/snapshot.rs::split_slot_chunks_covers_all_slots_in_order`
/// 常规覆盖);需重跑时用
/// `cargo test --test persist_contracts large_flush_splits_into_parallel_segments -- --ignored`。
#[test]
#[ignore = "规模用例:70k 行切块并行(debug 约 3 分钟)"]
fn large_flush_splits_into_parallel_segments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rows = 70_000_usize;
    let dimension = 8_u32;
    let db = Builder::default()
        .dimension(dimension)
        .path(dir.path())
        .build()
        .expect("build");
    let ns = db.namespace("split");
    let mut written = 0;
    while written < rows {
        let take = 10_000.min(rows - written);
        let batch: Vec<Record> = (0..take)
            .map(|offset| {
                let row = written + offset;
                Record::new(vec![row as f32, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])
                    .key(format!("k{row}"))
            })
            .collect();
        ns.insert_batch(batch).expect("insert_batch");
        written += take;
    }
    db.flush().expect("flush");
    // 默认块行数 65_536:70_000 行切 2 块(65_536 + 4_464),并行度默认 min(核数, 8)。
    let segments = db.stats().expect("stats").segments.len();
    assert!(
        (1..=2).contains(&segments),
        "切块并行后段数应在 1..=2,实际 {segments}"
    );
    assert!(db.check().expect("check").ok);
    for row in (0..rows).step_by(997) {
        assert!(ns.get(&format!("k{row}")).expect("get").is_some());
    }
    db.close().expect("close");

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("split");
    assert!(ns.get("k69999").expect("get tail").is_some());
    let hits = ns
        .search()
        .vector(&[0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])
        .top_k(10)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 10);
    db.close().expect("close");
}
