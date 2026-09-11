//! L5 生命周期层契约验收:多段增量 flush、compaction、delta 区、WAL 轮转与多图检索。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-LIFE-PRE-001(compaction 策略校验)
//! * FC-LIFE-INV-008(段数上界)、FC-LIFE-INV-010(崩溃原子)、FC-LIFE-INV-017(快照一致)
//! * FC-LIFE-INV-023(自动遗忘默认关闭、删除可审计)
//! * FC-LIFE-STA-001(合并状态机)、FC-LIFE-ERR-001(运行期失败回 Idle)
//! * FC-LIFE-POST-004(读路径访问攒批)
//! * FC-LIFE-POST-005(命名空间规范化)、FC-LIFE-POST-006(注销持久化)
//! * FC-LIFE-POST-007(快照统计)、FC-LIFE-POST-008(硬链接备份)、FC-LIFE-POST-009(fsck 建议)
//! * FC-LIFE-CPLX-001(TTL 块级剪枝)、FC-LIFE-CPLX-003/004(compaction 复杂度与段数上界)
//! * FC-LIFE-CPLX-006(后台维护单轮复杂度)
//! * FC-MODEL-POST-004(history_horizon 回收)、FC-MODEL-POST-007(反向关系表)
//! * FC-LIFE-POST-003(增量段 flush)、FC-PERSIST-STA-004(多段 MANIFEST 提交)
//! * FC-PERSIST-POST-010(delta 区往返/回放)、FC-PERSIST-POST-011(WAL 轮转)
//! * FC-INDEX-INV-008(多段 ANN + 未落盘尾归并 ≡ 全量暴力)

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mneme::{Builder, CompactionPolicy, FsyncHook, IoAction, Mneme, Record, UpdatePatch};

mod common;

use common::FakeClock;

/// 在临时目录新建持久库。
fn build(dir: &std::path::Path, dimension: u32) -> Mneme {
    Builder::default()
        .dimension(dimension)
        .path(dir)
        .build()
        .expect("build")
}

/// FC-LIFE-POST-003 / FC-PERSIST-STA-004:第二次 flush 生成**新段**并保留旧段;
/// 重开后跨段数据完整、`check()` 通过。
#[test]
fn incremental_flush_appends_segments() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        db.flush().expect("first flush");
        ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
        db.flush().expect("second flush");

        let stats = db.stats().expect("stats");
        assert_eq!(stats.segments.len(), 2, "增量 flush 必须追加新段而非替换");
        // 段号连续且旧段仍被 MANIFEST 引用。
        assert_eq!(stats.segments[0].id.get(), 0);
        assert_eq!(stats.segments[1].id.get(), 1);
        assert!(stats.segments[0].index_nodes > 0, "第一段应有 hidx 索引");
        assert!(stats.segments[1].index_nodes > 0, "第二段应有 hidx 索引");
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert!(ns.get("a").expect("a").is_some());
    assert!(ns.get("b").expect("b").is_some());
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// FC-LIFE-POST-003:增量 flush 不改写已提交旧段(write-once)。
#[test]
fn incremental_flush_does_not_rewrite_committed_segments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = build(dir.path(), 2);
    let ns = db.namespace("demo");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
    db.flush().expect("first flush");
    let first = dir.path().join("segments").join("seg_000000.vsec");
    let before = std::fs::read(&first).expect("read seg");

    ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
    db.flush().expect("second flush");
    let after = std::fs::read(&first).expect("read seg again");
    assert_eq!(before, after, "已提交旧段绝不能被增量 flush 改写");
    db.close().expect("close");
}

/// FC-PERSIST-POST-010:已落盘记录的 `touch`(无 boost)与关系变更经 delta 区
/// 持久化,重开后访问统计与入/出边不丢。
#[test]
fn delta_access_and_relations_survive_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .clock(std::sync::Arc::new(clock.clone()))
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        let a = ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        let b = ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
        db.flush().expect("flush base records");
        let a = match a {
            mneme::InsertOutcome::Inserted(id) | mneme::InsertOutcome::Merged(id) => id,
            other => panic!("期望写入,得到 {other:?}"),
        };
        let b = match b {
            mneme::InsertOutcome::Inserted(id) | mneme::InsertOutcome::Merged(id) => id,
            other => panic!("期望写入,得到 {other:?}"),
        };
        // 不产生新版本:访问计数与关系变更只能经 delta 区落盘。
        clock.set(2_000);
        assert!(ns.touch_by_rowid(a, None).expect("touch"));
        ns.relate(a, b, mneme::RelationKind::RELATED, 0.5)
            .expect("relate");
        db.flush().expect("flush delta");
        assert_eq!(
            db.stats().expect("stats").segments.len(),
            2,
            "delta 段也应追加进 MANIFEST"
        );
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert_eq!(
        ns.count(Some(mneme::Expr::field("access_count").eq(1)))
            .expect("count"),
        1,
        "delta Access 必须恢复访问计数"
    );
    let a = ns.get("a").expect("get a").expect("a 存在").rowid();
    let b = ns.get("b").expect("get b").expect("b 存在").rowid();
    let edges = ns
        .neighbors(a, &[mneme::RelationKind::RELATED])
        .expect("neighbors");
    assert_eq!(edges.len(), 1, "delta Relate 必须恢复出边");
    assert_eq!(edges[0].to, b);
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// FC-PERSIST-POST-011:WAL 达 `wal_file_bytes` 后轮转出新文件(单批不跨文件),
/// 重开时逐文件回放,数据无丢失。
#[test]
fn wal_rotation_splits_files_without_losing_batches() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = Builder::default()
            .dimension(8)
            .path(dir.path())
            .compaction(CompactionPolicy {
                wal_file_bytes: 512,
                ..CompactionPolicy::default()
            })
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        for row in 0..200_u32 {
            let vector: Vec<f32> = (0..8).map(|col| (row + col) as f32).collect();
            ns.insert(Record::new(vector).key(format!("k{row}")))
                .expect("insert");
        }
        let wal_files: Vec<String> = std::fs::read_dir(dir.path().join("wal"))
            .expect("read wal dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("wal_"))
            .collect();
        assert!(
            wal_files.len() > 1,
            "WAL 超过 512B 后必须轮转出多个文件,实际:{wal_files:?}"
        );
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    for row in 0..200_u32 {
        assert!(
            ns.get(&format!("k{row}")).expect("get").is_some(),
            "WAL 轮转回放不得丢记录 k{row}"
        );
    }
    db.close().expect("close");
}

/// FC-INDEX-INV-008:两段各带 hidx,查询 = 多图 ANN + 未落盘尾暴力归并,
/// 大 `ef` 下与全量暴力结果一致。
#[test]
fn multi_segment_search_matches_bruteforce() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .dimension(4)
        .path(dir.path())
        .tuning(mneme::Tuning {
            brute_force_max_rows: 8,
            ..mneme::Tuning::default()
        })
        .build()
        .expect("build");
    let ns = db.namespace("demo");
    let batch_a: Vec<Record> = (0..32_u32)
        .map(|row| Record::new(vec![(row % 7) as f32, 1.0, 0.0, 0.0]).key(format!("a{row}")))
        .collect();
    ns.insert_batch(batch_a).expect("batch a");
    db.flush().expect("flush a");
    let batch_b: Vec<Record> = (0..32_u32)
        .map(|row| Record::new(vec![0.0, (row % 7) as f32, 1.0, 0.0]).key(format!("b{row}")))
        .collect();
    ns.insert_batch(batch_b).expect("batch b");
    db.flush().expect("flush b");
    assert_eq!(db.stats().expect("stats").segments.len(), 2);

    let query = vec![1.0_f32, 1.0, 1.0, 0.0];
    let ann: Vec<String> = ns
        .search()
        .vector(&query)
        .top_k(10)
        .ef(256)
        .execute()
        .expect("search")
        .into_iter()
        .filter_map(|hit| hit.key.as_ref().map(ToString::to_string))
        .collect();

    // 参照:同数据改为恒暴力(阈值拉满),结果集合必须一致(ef 足够大)。
    let db_brute = Builder::default()
        .dimension(4)
        .tuning(mneme::Tuning {
            brute_force_max_rows: u32::MAX,
            ..mneme::Tuning::default()
        })
        .build()
        .expect("brute build");
    let ns_brute = db_brute.namespace("demo");
    for row in 0..32_u32 {
        ns_brute
            .insert(Record::new(vec![(row % 7) as f32, 1.0, 0.0, 0.0]).key(format!("a{row}")))
            .expect("a");
    }
    for row in 0..32_u32 {
        ns_brute
            .insert(Record::new(vec![0.0, (row % 7) as f32, 1.0, 0.0]).key(format!("b{row}")))
            .expect("b");
    }
    let brute: Vec<String> = ns_brute
        .search()
        .vector(&query)
        .top_k(10)
        .execute()
        .expect("brute search")
        .into_iter()
        .filter_map(|hit| hit.key.as_ref().map(ToString::to_string))
        .collect();
    assert_eq!(ann, brute, "多段 ANN 结果必须与全量暴力一致(ef→∞ 语义)");
    db.close().expect("close");
}

/// FC-LIFE-PRE-001:compaction 策略非法值建库即拒绝(避免触发条件永假/除零)。
#[test]
fn builder_rejects_invalid_compaction_policy() {
    let invalid = [
        CompactionPolicy {
            tier_ratio: 1,
            ..CompactionPolicy::default()
        },
        CompactionPolicy {
            tier_count: 1,
            ..CompactionPolicy::default()
        },
        CompactionPolicy {
            segment_rows: 0,
            ..CompactionPolicy::default()
        },
        CompactionPolicy {
            dead_ratio: f32::NAN,
            ..CompactionPolicy::default()
        },
        CompactionPolicy {
            io_budget: 1.5,
            ..CompactionPolicy::default()
        },
    ];
    for policy in invalid {
        assert!(
            matches!(
                Builder::default().dimension(2).compaction(policy).build(),
                Err(mneme::MnemeError::Config { .. })
            ),
            "非法 compaction 策略必须返回 Config:{policy:?}"
        );
    }
}

/// 小段分级策略(4 行一段、2 段一层),便于确定性触发合并。
fn tiered_policy() -> CompactionPolicy {
    CompactionPolicy {
        tier_ratio: 2,
        tier_count: 2,
        segment_rows: 4,
        ..CompactionPolicy::default()
    }
}

/// FC-LIFE-INV-008 / FC-LIFE-CPLX-003 / FC-LIFE-CPLX-004:同层攒够阈值即合并,
/// 段数回落;重开后数据完整。
#[test]
fn compaction_bounds_segment_count() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .compaction(tiered_policy())
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        for batch in 0..3_u32 {
            let records: Vec<Record> = (0..4_u32)
                .map(|row| {
                    Record::new(vec![(batch * 4 + row) as f32, 1.0])
                        .key(format!("k{}", batch * 4 + row))
                })
                .collect();
            ns.insert_batch(records).expect("batch");
            db.flush().expect("flush");
        }
        assert_eq!(db.stats().expect("stats").segments.len(), 3);
        db.compact().expect("compact");
        let stats = db.stats().expect("stats");
        assert_eq!(
            stats.segments.len(),
            2,
            "同层 3 段攒够 2 后必须合并为 1 个上层段"
        );
        assert_eq!(
            stats.compaction,
            mneme::CompactionState::Idle,
            "提交后必须回到 Idle"
        );
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    for key in 0..12_u32 {
        assert!(
            ns.get(&format!("k{key}")).expect("get").is_some(),
            "合并不得丢记录 k{key}"
        );
    }
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// FC-MODEL-POST-004:有限 `history_horizon` 下,超期墓碑整链物理回收;
/// 最新活版本始终保留,重开后删除不复活。
#[test]
fn compaction_reclaims_tombstones_under_horizon() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    let policy = CompactionPolicy {
        history_horizon: Some(Duration::from_millis(0)),
        ..CompactionPolicy::default()
    };
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .compaction(policy)
            .clock(Arc::new(clock.clone()))
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
        clock.set(2_000);
        assert!(ns.delete("a").expect("delete"));
        db.flush().expect("flush");
        clock.set(3_000);
        db.compact().expect("compact");

        let stats = db.stats().expect("stats");
        assert_eq!(stats.segments.len(), 1, "死比率超线应重写该段");
        assert_eq!(stats.segments[0].rows, 1, "超期墓碑整链物理回收");
        assert_eq!(stats.history.reclaimed_versions, 2, "回收 a 的两个版本");
        assert!(!ns.exists("a").expect("exists"), "删除后当前不可见");
        assert!(ns.exists("b").expect("exists"));
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert!(!ns.exists("a").expect("exists"), "墓碑不得复活");
    assert!(ns.exists("b").expect("exists"));
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// FC-LIFE-INV-017 / FC-MODEL-POST-004:窗口内历史版本不被回收,
/// 快照钉住的视图跨 compaction 不变。
#[test]
fn compaction_preserves_snapshot_and_as_of_within_horizon() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    let policy = CompactionPolicy {
        history_horizon: Some(Duration::from_secs(3_600)),
        tier_ratio: 2,
        tier_count: 2,
        segment_rows: 4,
        // 关掉死比率分支,专测同层合并。
        dead_ratio: 1.0,
        ..CompactionPolicy::default()
    };
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .compaction(policy)
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("demo");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
    db.flush().expect("flush v1");
    let snapshot = db.snapshot();

    clock.set(2_000);
    ns.update("a", UpdatePatch::new().importance(0.9))
        .expect("update");
    ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
    db.flush().expect("flush v2");
    clock.set(3_000);
    db.compact().expect("compact");
    assert_eq!(
        db.stats().expect("stats").segments.len(),
        1,
        "同层 2 段应被合并(证明确实发生 compaction)"
    );

    let snap_ns = snapshot.namespace("demo");
    let snap_record = snap_ns.get("a").expect("snapshot get").expect("存在");
    assert!(
        (snap_record.importance() - 0.5).abs() < 1e-6,
        "快照必须钉住取快照时的旧版本(importance=0.5),实际 {}",
        snap_record.importance()
    );
    assert!(
        db.as_of(1_500)
            .expect("as_of")
            .namespace("demo")
            .get("a")
            .expect("get")
            .is_some(),
        "窗口内历史版本不得被 compaction 回收"
    );
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// FC-MODEL-POST-004:窗口外的历史版本在 compaction 后不可回溯;
/// 最新版本与当前读不受影响。
#[test]
fn history_horizon_reclaims_old_versions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    let policy = CompactionPolicy {
        history_horizon: Some(Duration::from_millis(1_000)),
        tier_ratio: 2,
        tier_count: 2,
        segment_rows: 4,
        ..CompactionPolicy::default()
    };
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .compaction(policy)
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("demo");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
    clock.set(2_000);
    ns.update("a", UpdatePatch::new().importance(0.9))
        .expect("update");
    db.flush().expect("flush");

    assert!(
        db.as_of(1_500)
            .expect("as_of")
            .namespace("demo")
            .get("a")
            .expect("get")
            .is_some(),
        "回收前窗口内历史可读"
    );
    clock.set(5_000);
    db.compact().expect("compact");
    assert!(
        db.as_of(1_500)
            .expect("as_of")
            .namespace("demo")
            .get("a")
            .expect("get")
            .is_none(),
        "超期历史版本 compaction 后不可回溯"
    );
    assert!(db.stats().expect("stats").history.reclaimed_versions >= 1);
    db.close().expect("close");
}

/// FC-LIFE-STA-001:pause 时 compaction 不启动;resume 后恢复可合并。
#[test]
fn compaction_respects_pause() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .compaction(tiered_policy())
        .build()
        .expect("build");
    let ns = db.namespace("demo");
    for batch in 0..3_u32 {
        let records: Vec<Record> = (0..4_u32)
            .map(|row| Record::new(vec![row as f32, 1.0]).key(format!("k{}", batch * 4 + row)))
            .collect();
        ns.insert_batch(records).expect("batch");
        db.flush().expect("flush");
    }
    let control = db.compact_control();
    control.pause();
    db.compact().expect("paused compact");
    assert_eq!(db.stats().expect("stats").segments.len(), 3, "暂停不得合并");
    assert_eq!(
        db.stats().expect("stats").compaction,
        mneme::CompactionState::Idle,
        "非法转移(Idle 上 Resume/pause)不得残留 Running"
    );
    control.resume();
    db.compact().expect("compact");
    assert_eq!(db.stats().expect("stats").segments.len(), 2, "恢复后可合并");
    db.close().expect("close");
}

/// FC-LIFE-INV-010 / FC-LIFE-ERR-001:compaction 提交前 MANIFEST 写失败 →
/// 结构化错误、状态回 `Idle`、活跃段集与数据不变;孤儿新段由重开清理。
#[test]
fn compaction_failure_keeps_state_and_returns_idle() {
    /// 前 `ok_manifests` 次 MANIFEST 写放行,之后注入失败。
    struct FailAfter {
        ok_manifests: usize,
        seen: AtomicUsize,
    }
    impl FsyncHook for FailAfter {
        fn before(&self, action: IoAction<'_>) -> std::io::Result<()> {
            if let IoAction::Write { file, .. } = action
                && file.starts_with("MANIFEST.")
            {
                let seen = self.seen.fetch_add(1, Ordering::Relaxed);
                if seen >= self.ok_manifests {
                    return Err(std::io::Error::other(
                        "injected compaction manifest failure",
                    ));
                }
            }
            Ok(())
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .compaction(tiered_policy())
            .fsync_hook(Arc::new(FailAfter {
                ok_manifests: 3,
                seen: AtomicUsize::new(0),
            }))
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        for batch in 0..3_u32 {
            let records: Vec<Record> = (0..4_u32)
                .map(|row| Record::new(vec![row as f32, 1.0]).key(format!("k{}", batch * 4 + row)))
                .collect();
            ns.insert_batch(records).expect("batch");
            db.flush().expect("flush");
        }
        assert!(db.compact().is_err(), "注入的 MANIFEST 失败必须上报");
        let stats = db.stats().expect("stats");
        assert_eq!(stats.segments.len(), 3, "失败提交不得改变活跃段集");
        assert_eq!(stats.compaction, mneme::CompactionState::Idle);
        for key in 0..12_u32 {
            assert!(ns.get(&format!("k{key}")).expect("get").is_some());
        }
        // close 释放独占锁(失败 compaction 未重置 WAL,无新槽位时为无操作)。
        db.close().expect("close");
    }

    // 无 hook 重开:孤儿新段被清理,旧段集完好。
    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    for key in 0..12_u32 {
        assert!(ns.get(&format!("k{key}")).expect("get").is_some());
    }
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// FC-LIFE-CPLX-001:多段增量段写入 `ttl_map` 后重开,TTL 可见性语义不变
/// (整块未过期时零逐行判定;过期行不可见)。
#[test]
fn ttl_expiry_survives_multi_segment_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .clock(Arc::new(clock.clone()))
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        ns.insert(
            Record::new(vec![1.0, 0.0])
                .key("short")
                .ttl(Duration::from_millis(100)),
        )
        .expect("short");
        db.flush().expect("flush short");
        ns.insert(
            Record::new(vec![0.0, 1.0])
                .key("long")
                .ttl(Duration::from_secs(3_600)),
        )
        .expect("long");
        db.flush().expect("flush long");
        assert_eq!(db.stats().expect("stats").segments.len(), 2);

        clock.set(2_000);
        assert!(!ns.exists("short").expect("exists"), "短 TTL 已过期");
        assert!(ns.exists("long").expect("exists"));
        db.close().expect("close");
    }

    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .clock(Arc::new(clock))
        .build()
        .expect("reopen");
    let ns = db.namespace("demo");
    assert!(!ns.exists("short").expect("exists"), "重开后过期语义不变");
    assert!(ns.exists("long").expect("exists"));
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// 轮询等待 `cond` 成立(后台维护为异步;注入时钟决定触发,真实时间只驱动线程)。
fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

/// FC-LIFE-POST-004:读路径命中攒批后落 WAL;崩溃(未 close)重开后访问计数仍在。
#[test]
fn access_hits_are_batched_and_flushed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .access_flush_interval(Duration::from_millis(20))
            .clock(Arc::new(clock.clone()))
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        db.flush().expect("flush");

        let hits = ns
            .search()
            .vector(&[1.0, 0.0])
            .top_k(1)
            .execute()
            .expect("search");
        assert_eq!(hits.len(), 1);
        // 时钟推进越过 access_flush_interval,后台维护把缓冲合并并落 WAL。
        clock.set(2_000);
        assert!(
            wait_until(Duration::from_secs(3), || {
                ns.count(Some(mneme::Expr::field("access_count").ge(1)))
                    .expect("count")
                    == 1
            }),
            "读命中必须攒批落盘"
        );
    } // 不 close:模拟崩溃,只靠 WAL 回放

    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .clock(Arc::new(clock))
        .build()
        .expect("reopen");
    let ns = db.namespace("demo");
    assert_eq!(
        ns.count(Some(mneme::Expr::field("access_count").ge(1)))
            .expect("count"),
        1,
        "崩溃前落盘的访问计数必须由 WAL 回放恢复"
    );
    db.close().expect("close");
}

/// FC-LIFE-INV-023:自动遗忘默认关闭(安全默认),`stats().retain` 恒 `None`。
#[test]
fn auto_retention_is_off_by_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .retain_interval(Duration::from_millis(20))
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("demo");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a").importance(0.0))
        .expect("a");
    clock.set(10_000_000);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        db.stats().expect("stats").retain.is_none(),
        "未显式开启 retention 时绝不允许自动遗忘"
    );
    assert!(ns.exists("a").expect("exists"), "记录不得被静默删除");
    db.close().expect("close");
}

/// FC-LIFE-INV-023 / FC-LIFE-CPLX-006:显式开启自动遗忘后按周期执行,
/// 报告可审计且删除可经 `iter_with(_, true)` 追溯。
#[test]
fn auto_retention_forgets_expired_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .retention(Some(
            mneme::Retention::new()
                .half_life(Duration::from_millis(1_000))
                .min_importance(0.9),
        ))
        .retain_interval(Duration::from_millis(20))
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("demo");
    ns.insert(Record::new(vec![1.0, 0.0]).key("drop").importance(0.0))
        .expect("drop");
    ns.insert(Record::new(vec![0.0, 1.0]).key("keep").importance(1.0))
        .expect("keep");
    // 时钟快进:低重要度记录保留分跌破阈值;高分记录在快进后访问一次,
    // 衰减时钟被刷新,保留分维持高位。
    clock.set(1_000_000);
    ns.touch("keep", None).expect("touch keep");
    assert!(
        wait_until(Duration::from_secs(3), || {
            db.stats()
                .expect("stats")
                .retain
                .is_some_and(|report| report.forgotten >= 1)
        }),
        "开启自动遗忘后必须产生可审计报告"
    );
    assert!(!ns.exists("drop").expect("exists"), "低分记录被遗忘");
    assert!(ns.exists("keep").expect("exists"), "高分记录保留");
    let audited = ns
        .iter_with(None, true)
        .expect("iter_with")
        .filter_map(Result::ok)
        .filter(|record| record.key() == Some("drop"))
        .count();
    assert!(audited >= 1, "墓碑必须可经 include_deleted 审计");
    db.close().expect("close");
}

/// FC-LIFE-INV-008 / FC-LIFE-CPLX-006:后台自动 compaction 按 tier 触发,
/// 段数回落且数据完整。
#[test]
fn auto_compaction_triggers_in_background() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .compaction(tiered_policy())
        // 借用 retain_interval 作为维护压缩检查节拍(测试加速)。
        .retain_interval(Duration::from_millis(20))
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("demo");
    for batch in 0..3_u32 {
        let records: Vec<Record> = (0..4_u32)
            .map(|row| Record::new(vec![row as f32, 1.0]).key(format!("k{}", batch * 4 + row)))
            .collect();
        ns.insert_batch(records).expect("batch");
        db.flush().expect("flush");
    }
    assert_eq!(db.stats().expect("stats").segments.len(), 3);
    clock.set(2_000);
    assert!(
        wait_until(Duration::from_secs(3), || {
            db.stats().expect("stats").segments.len() == 2
        }),
        "后台维护必须自动触发同层合并"
    );
    for key in 0..12_u32 {
        assert!(ns.get(&format!("k{key}")).expect("get").is_some());
    }
    db.close().expect("close");
}

/// FC-LIFE-POST-005:路径规范化(去首尾 `/`、合并 `//`)、按段边界级联,
/// 且 `drop_namespace` 返回墓碑行数。
#[test]
fn namespace_paths_are_normalized_and_boundary_matched() {
    let db = Mneme::in_memory(2).expect("in_memory");
    db.namespace("//a//b/")
        .insert(Record::new(vec![1.0, 0.0]).key("one"))
        .expect("insert a/b");
    db.namespace("a/bc")
        .insert(Record::new(vec![0.0, 1.0]).key("other"))
        .expect("insert a/bc");
    assert_eq!(
        db.list_namespaces().expect("list"),
        vec!["a/b".to_string(), "a/bc".to_string()]
    );

    // `drop_namespace("a/b")` 含子空间但不含兄弟 `a/bc`;返回墓碑行数。
    assert_eq!(db.drop_namespace("a/b").expect("drop a/b"), 1);
    assert_eq!(
        db.list_namespaces().expect("list"),
        vec!["a/bc".to_string()],
        "按 `/` 段边界匹配:`a/bc` 不属于 `a/b`"
    );
    assert!(db.namespace("a/bc").exists("other").expect("exists"));
    assert!(db.namespace("a/b").get("one").expect("get").is_none());
    // 保留其父级 `a/b` 的兄弟不受影响;再删父级 `a` 级联剩余子空间。
    assert_eq!(db.drop_namespace("a").expect("drop a"), 1);
    assert!(db.list_namespaces().expect("list").is_empty());
}

/// FC-LIFE-POST-005:`namespace()` 不返回 `Result`,超深路径在首次写入时
/// 以 `Config` 报告,绝不静默截断或 panic。
#[test]
fn namespace_depth_limit_reported_at_first_write() {
    let db = Mneme::in_memory(2).expect("in_memory");
    let deep = (0..33)
        .map(|index| format!("s{index}"))
        .collect::<Vec<_>>()
        .join("/");
    let ns = db.namespace(&deep);
    assert!(matches!(
        ns.insert(Record::new(vec![1.0, 0.0])),
        Err(mneme::MnemeError::Config { .. })
    ));
    assert!(
        db.list_namespaces().expect("list").is_empty(),
        "失败写入不登记"
    );
}

/// FC-LIFE-POST-006:命名空间注销经 WAL 持久化;未 flush 即崩溃重开后,
/// 已注销路径不复活,`NsId` 水位不回退。
#[test]
fn drop_namespace_is_durable_across_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("a")
            .insert(Record::new(vec![1.0, 0.0]).key("x"))
            .expect("x");
        db.namespace("a/b")
            .insert(Record::new(vec![0.0, 1.0]).key("y"))
            .expect("y");
        assert_eq!(db.drop_namespace("a").expect("drop"), 2, "级联墓碑两行");
        assert!(db.list_namespaces().expect("list").is_empty());
    } // 不 close:注销只存在于 WAL

    let db = Mneme::open(dir.path()).expect("reopen");
    assert!(
        db.list_namespaces().expect("list").is_empty(),
        "崩溃恢复后已注销命名空间不得复活"
    );
    assert!(db.namespace("a").get("x").expect("get").is_none());
    // 复用原路径:拿到新 NsId 后一切正常(旧 NsId 不复用)。
    db.namespace("a")
        .insert(Record::new(vec![1.0, 0.0]).key("x"))
        .expect("reinsert");
    assert_eq!(db.list_namespaces().expect("list"), vec!["a".to_string()]);
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// FC-LIFE-POST-007:快照统计钉住视图;后续写入/flush 不改变已取快照统计,
/// 且批量 `RowId` 点读可用。
#[test]
fn snapshot_stats_pin_view() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = build(dir.path(), 2);
    let ns = db.namespace("demo");
    let a = ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
    let a = match a {
        mneme::InsertOutcome::Inserted(id) | mneme::InsertOutcome::Merged(id) => id,
        other => panic!("期望写入,得到 {other:?}"),
    };
    ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
    db.flush().expect("flush");

    let snapshot = db.snapshot();
    let before = snapshot.stats();
    assert_eq!(before.segments, 1);
    assert_eq!(before.rows, 2);
    assert!(before.version >= 2, "基线水位随写入单调递增");
    let snap_ns = snapshot.namespace("demo");
    let refs = snap_ns.get_many_by_rowid(&[a]).expect("batch rowid");
    assert!(refs[0].is_some(), "快照批量 RowId 点读必须命中");

    ns.insert(Record::new(vec![1.0, 1.0]).key("c")).expect("c");
    db.flush().expect("flush");
    assert_eq!(snapshot.stats(), before, "快照统计不得随后续写入变化");
    assert!(db.snapshot().stats().rows > before.rows);
    db.close().expect("close");
}

/// FC-LIFE-POST-008:同盘备份走硬链接(段文件同 inode),产物仍可独立打开。
#[test]
fn backup_hardlinks_segments_when_possible() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backup_dir = tempfile::tempdir().expect("backup tempdir");
    let backup_path = backup_dir.path().join("copy");
    {
        let db = build(dir.path(), 2);
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        db.flush().expect("flush a");
        ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
        db.flush().expect("flush b");
        let report = db.backup_to(&backup_path).expect("backup");
        assert!(report.hardlinked, "同盘备份应走硬链接路径");
        assert!(report.files > 0 && report.bytes > 0);
        db.close().expect("close");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let source = dir.path().join("segments").join("seg_000000.vsec");
        let copied = backup_path.join("segments").join("seg_000000.vsec");
        let source_links = std::fs::metadata(&source).expect("source meta").nlink();
        let copy_links = std::fs::metadata(&copied).expect("copy meta").nlink();
        assert!(source_links > 1 && copy_links > 1, "硬链接应共享 inode");
    }

    let backup = Mneme::open(&backup_path).expect("open backup");
    assert!(backup.check().expect("check").ok);
    assert!(backup.namespace("demo").get("b").expect("get").is_some());
    backup.close().expect("close");
}

/// FC-LIFE-POST-009 / 查询延迟直方图:stats 报告真实死比率与延迟采样;
/// check() 给出合并建议但不把健康库判为损坏。
#[test]
fn stats_report_latency_dead_ratio_and_fsck_suggestions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = build(dir.path(), 2);
    let ns = db.namespace("demo");
    for key in 0..4_u32 {
        ns.insert(Record::new(vec![key as f32, 1.0]).key(format!("k{key}")))
            .expect("insert");
    }
    db.flush().expect("flush");
    for _ in 0..2 {
        let _ = ns
            .search()
            .vector(&[1.0, 0.0])
            .top_k(2)
            .execute()
            .expect("search");
    }
    assert!(ns.delete("k0").expect("delete"));
    assert!(ns.delete("k1").expect("delete"));

    let stats = db.stats().expect("stats");
    assert_eq!(stats.segments.len(), 1);
    assert!(
        stats.segments[0].dead_ratio >= 0.25,
        "死比率应真实反映墓碑+被遮蔽版本"
    );
    let samples: u64 = stats.query_latency.buckets().iter().sum();
    assert!(samples >= 2, "每次 execute 必须采样一次延迟");

    let report = db.check().expect("check");
    assert!(report.ok, "墓碑不构成损坏");
    assert!(
        report.suggestions.iter().any(|line| line.contains("建议")),
        "fsck 应给出运维建议:{:?}",
        report.suggestions
    );
    db.close().expect("close");
}

/// FC-MODEL-POST-007:`RelationIndex::Both` 时段内写反向表;重开后入边/出边
/// 与默认 `Outgoing` 模式逐边一致。
#[test]
fn reverse_relation_table_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .relation_index(mneme::RelationIndex::Both)
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        let a = ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        let b = ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
        let a = match a {
            mneme::InsertOutcome::Inserted(id) | mneme::InsertOutcome::Merged(id) => id,
            other => panic!("期望写入,得到 {other:?}"),
        };
        let b = match b {
            mneme::InsertOutcome::Inserted(id) | mneme::InsertOutcome::Merged(id) => id,
            other => panic!("期望写入,得到 {other:?}"),
        };
        ns.relate(a, b, mneme::RelationKind::RELATED, 0.5)
            .expect("relate");
        db.flush().expect("flush");
        db.close().expect("close");
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    let a = ns.get("a").expect("a").expect("存在").rowid();
    let b = ns.get("b").expect("b").expect("存在").rowid();
    let out = ns
        .neighbors(a, &[mneme::RelationKind::RELATED])
        .expect("out");
    let incoming = ns
        .predecessors(b, &[mneme::RelationKind::RELATED])
        .expect("in");
    assert_eq!(out.len(), 1, "出边必须恢复");
    assert_eq!(out[0].to, b);
    assert_eq!(incoming.len(), 1, "反向表恢复的入边必须存在");
    assert_eq!(incoming[0].from, a);
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}
