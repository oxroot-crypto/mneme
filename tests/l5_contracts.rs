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
//! * FC-MODEL-POST-004(history_horizon 回收)、FC-MODEL-POST-005(入边)、FC-MODEL-POST-007(反向关系表)
//! * FC-LIFE-POST-003(增量段 flush)、FC-PERSIST-STA-004(多段 MANIFEST 提交)
//! * FC-PERSIST-POST-010(delta 区往返/回放)、FC-PERSIST-POST-011(WAL 轮转)
//! * FC-PERSIST-POST-012(槽位归属恢复)
//! * FC-PERSIST-ERR-006(损坏段原地跳过)
//! * FC-PERSIST-CPLX-011(增量 flush 复杂度)
//! * FC-INDEX-INV-008(多段 ANN + 未落盘尾归并 ≡ 全量暴力)

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
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
        let error = db.compact().expect_err("注入的 MANIFEST 失败必须上报");
        assert!(
            matches!(
                error,
                mneme::MnemeError::Io(_) | mneme::MnemeError::Corrupted { .. }
            ),
            "运行期失败必须以 Io/Corrupted 结构化上报,实际 {error:?}"
        );
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
        // 同一条记录再命中一次:缓冲按键累加,攒批落盘后计数必须是 2。
        let hits = ns
            .search()
            .vector(&[1.0, 0.0])
            .top_k(1)
            .execute()
            .expect("search again");
        assert_eq!(hits.len(), 1);
        // 手动推进一轮维护(确定性;不依赖后台线程调度):缓冲合并并落 WAL。
        clock.set(2_000);
        db.maintenance_tick().expect("maintenance tick");
        assert_eq!(
            ns.count(Some(mneme::Expr::field("access_count").eq(2)))
                .expect("count"),
            1,
            "读命中必须攒批落盘(同 RowId 累加)"
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
        ns.count(Some(mneme::Expr::field("access_count").eq(2)))
            .expect("count"),
        1,
        "崩溃前落盘的访问计数必须由 WAL 回放恢复(合并后的累计值)"
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
    db.maintenance_tick().expect("maintenance tick");
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
    // 手动推进一轮维护(确定性;不依赖后台线程调度)。
    db.maintenance_tick().expect("maintenance tick");
    assert!(
        db.stats()
            .expect("stats")
            .retain
            .is_some_and(|report| report.forgotten >= 1),
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
    // 手动推进一轮维护(确定性;不依赖后台线程调度)。
    clock.set(2_000);
    db.maintenance_tick().expect("maintenance tick");
    assert_eq!(
        db.stats().expect("stats").segments.len(),
        2,
        "维护单轮必须触发同层合并"
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
    // 有限 horizon 才会给出"建议合并回收":默认 None 时没有任何版本可回收,
    // 建议会与实际触发口径矛盾(FC-LIFE-POST-009)。
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .compaction(CompactionPolicy {
            history_horizon: Some(Duration::from_secs(3_600)),
            ..CompactionPolicy::default()
        })
        .build()
        .expect("build");
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

/// FC-LIFE-POST-003:无新增槽位与 delta 时 flush 为空操作(不产段、不推进段号)。
#[test]
fn empty_flush_is_noop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = build(dir.path(), 2);
    let ns = db.namespace("demo");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
    db.flush().expect("flush");
    let before = db.stats().expect("stats");
    db.flush().expect("flush again");
    db.flush().expect("flush third");
    let after = db.stats().expect("stats");
    assert_eq!(
        after.segments.len(),
        before.segments.len(),
        "无新增/无 delta 的 flush 必须为空操作"
    );
    assert_eq!(after.segments[0].id, before.segments[0].id);
    db.close().expect("close");
}

/// FC-PERSIST-POST-010:delta 之后的更新版本已把访问增量写进版本行快照,
/// 重开恢复不得重复累加(否则 `access_count` 虚高)。
#[test]
fn access_delta_is_not_double_counted_after_later_version() {
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
        let a = ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        let a = match a {
            mneme::InsertOutcome::Inserted(id) | mneme::InsertOutcome::Merged(id) => id,
            other => panic!("期望写入,得到 {other:?}"),
        };
        db.flush().expect("flush base");
        clock.set(2_000);
        assert!(ns.touch_by_rowid(a, None).expect("touch"));
        db.flush().expect("flush delta");
        clock.set(3_000);
        // 更新产生新版本行,其 access 列携带 touch 后的累计值 1。
        ns.update("a", UpdatePatch::new().importance(0.9))
            .expect("update");
        db.flush().expect("flush update");
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert_eq!(
        ns.count(Some(mneme::Expr::field("access_count").eq(1)))
            .expect("count"),
        1,
        "touch 一次后访问计数必须恒为 1"
    );
    assert_eq!(
        ns.count(Some(mneme::Expr::field("access_count").ge(2)))
            .expect("count"),
        0,
        "delta 不得与后续版本行的累计快照重复累加"
    );
    db.close().expect("close");
}

/// FC-LIFE-INV-008:默认 `history_horizon=None` 时没有任何可回收版本,
/// 死比率不得触发反复重写(否则永远回收不掉,形成无限写放大)。
#[test]
fn compact_without_horizon_does_not_rewrite_dead_segments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .clock(Arc::new(clock.clone()))
        .build()
        .expect("build");
    let ns = db.namespace("demo");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
    ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
    clock.set(2_000);
    assert!(ns.delete("a").expect("delete"));
    db.flush().expect("flush");
    let before = db.stats().expect("stats");
    for _ in 0..3 {
        db.compact().expect("compact");
    }
    let after = db.stats().expect("stats");
    assert_eq!(
        after.segments[0].id, before.segments[0].id,
        "无回收收益不得产出新段(默认 horizon=None)"
    );
    assert_eq!(after.history.reclaimed_versions, 0);
    assert!(!ns.exists("a").expect("exists"));
    assert!(ns.exists("b").expect("exists"));
    db.close().expect("close");
}

/// FC-LIFE-ERR-001:MANIFEST 提交后旧段清理失败不得报错(避免"磁盘已换、
/// 内存未换"半同步);孤儿旧段由重开清理,数据保持完整。
#[test]
fn compact_cleanup_failure_after_commit_still_succeeds() {
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
                .map(|row| Record::new(vec![row as f32, 1.0]).key(format!("k{}", batch * 4 + row)))
                .collect();
            ns.insert_batch(records).expect("batch");
            db.flush().expect("flush");
        }
        // 把 trash/ 换成普通文件:提交后的 move_to_trash 必然失败。
        let trash = dir.path().join("trash");
        std::fs::remove_dir_all(&trash).expect("remove trash");
        std::fs::write(&trash, b"blocked").expect("block trash");
        db.compact().expect("提交后清理失败仍应成功");
        assert_eq!(
            db.stats().expect("stats").segments.len(),
            2,
            "段集已原子替换"
        );
        for key in 0..12_u32 {
            assert!(ns.get(&format!("k{key}")).expect("get").is_some());
        }
        // 恢复目录形态,便于重开清理孤儿。
        std::fs::remove_file(&trash).expect("unblock trash");
        std::fs::create_dir(&trash).expect("recreate trash");
        db.close().expect("close");
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    for key in 0..12_u32 {
        assert!(ns.get(&format!("k{key}")).expect("get").is_some());
    }
    assert!(db.check().expect("check").ok);
    assert_eq!(db.stats().expect("stats").segments.len(), 2);
    db.close().expect("close");
}

/// FC-LIFE-STA-001:`Running` 中 `pause()` 转 `Paused`,在提交前中止并删除
/// 孤儿新段;`resume()` 后同一计划可正常提交。
#[test]
fn pause_during_running_aborts_before_commit() {
    /// 第一次写段文件时对控制句柄调用 `pause()`(模拟运行到提交前暂停)。
    struct PauseOnSegmentWrite {
        control: OnceLock<mneme::CompactionControl>,
        fired: std::sync::atomic::AtomicBool,
    }
    impl FsyncHook for PauseOnSegmentWrite {
        fn before(&self, action: IoAction<'_>) -> std::io::Result<()> {
            if let IoAction::Write { file, .. } = action
                && file.starts_with("segments/")
                && !self.fired.swap(true, Ordering::Relaxed)
                && let Some(control) = self.control.get()
            {
                control.pause();
            }
            Ok(())
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let hook = Arc::new(PauseOnSegmentWrite {
        control: OnceLock::new(),
        fired: std::sync::atomic::AtomicBool::new(false),
    });
    let hook_trait: Arc<dyn FsyncHook> = hook.clone();
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .compaction(tiered_policy())
        .fsync_hook(hook_trait)
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
    hook.fired.store(false, Ordering::Relaxed);
    hook.control.set(control.clone()).ok();
    control.resume();
    // 写新段时 hook 触发 pause → 在 MANIFEST 提交前中止。
    db.compact().expect("暂停中止不得报错");
    assert!(hook.fired.load(Ordering::Relaxed), "hook 必须被触发");
    let stats = db.stats().expect("stats");
    assert_eq!(stats.segments.len(), 3, "提交前中止不得改变段集");
    assert_eq!(stats.compaction, mneme::CompactionState::Idle);
    for key in 0..12_u32 {
        assert!(ns.get(&format!("k{key}")).expect("get").is_some());
    }

    // 恢复后同一计划必须成功提交(证明确实是暂停中止,而非计划未触发)。
    control.resume();
    db.compact().expect("resume 后合并");
    assert_eq!(db.stats().expect("stats").segments.len(), 2);
    db.close().expect("close");
}

/// FC-PERSIST-POST-011:Checkpoint 后已完全覆盖的旧 WAL 文件被删除,
/// 活跃 WAL 目录回到单文件。
#[test]
fn checkpoint_removes_covered_wal_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = CompactionPolicy {
        wal_file_bytes: 512,
        ..CompactionPolicy::default()
    };
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .compaction(policy)
        .build()
        .expect("build");
    let ns = db.namespace("demo");
    for batch in 0..8_u32 {
        let records: Vec<Record> = (0..4_u32)
            .map(|row| {
                Record::new(vec![(batch * 4 + row) as f32, 1.0])
                    .key(format!("k{}", batch * 4 + row))
            })
            .collect();
        ns.insert_batch(records).expect("batch");
    }
    let wal_dir = dir.path().join("wal");
    let rotated = std::fs::read_dir(&wal_dir).expect("read wal").count();
    assert!(rotated >= 2, "写入量应触发 WAL 轮转,实际 {rotated} 个文件");
    db.flush().expect("flush");
    let after = std::fs::read_dir(&wal_dir).expect("read wal").count();
    assert_eq!(after, 1, "Checkpoint 后旧 WAL 文件必须被删除");
    for key in 0..32_u32 {
        assert!(ns.get(&format!("k{key}")).expect("get").is_some());
    }
    db.close().expect("close");
}

/// FC-MODEL-POST-004:时钟回拨下最新墓碑在窗口外时整链回收,
/// 被删记录绝不因旧活版本残留而复活,`latest` 不悬挂。
#[test]
fn compaction_never_revives_deleted_record_under_clock_skew() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = CompactionPolicy {
        history_horizon: Some(Duration::from_millis(50)),
        ..tiered_policy()
    };
    // 第一段:活版本 tx=280。
    {
        let clock = FakeClock::default();
        clock.set(280);
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .compaction(policy)
            .clock(Arc::new(clock))
            .build()
            .expect("build");
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
        db.flush().expect("flush live");
        db.close().expect("close");
    }
    // 第二段:新进程时钟回拨到 100,删除产生早于活版本的墓碑 tx=100。
    {
        let clock = FakeClock::default();
        clock.set(100);
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .compaction(policy)
            .clock(Arc::new(clock))
            .build()
            .expect("reopen");
        assert!(db.namespace("demo").delete("a").expect("delete"));
        db.flush().expect("flush tombstone");
        db.close().expect("close");
    }
    // 第三段:时钟前进后 compaction。墓碑 tx=100 < cutoff=250(窗口外),
    // 整链(含 tx=280 的活版本)一并回收 —— 绝不只回收墓碑留下悬挂 latest。
    {
        let clock = FakeClock::default();
        clock.set(300);
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .compaction(policy)
            .clock(Arc::new(clock))
            .build()
            .expect("reopen2");
        db.compact().expect("compact");
        let stats = db.stats().expect("stats");
        assert_eq!(
            stats.history.reclaimed_versions, 2,
            "时钟回拨下必须整链回收(墓碑+活版本)"
        );
        assert!(!db.namespace("demo").exists("a").expect("exists"));
        db.close().expect("close");
    }

    let db = Mneme::open(dir.path()).expect("reopen3");
    assert!(
        !db.namespace("demo").exists("a").expect("exists"),
        "重开后被删记录绝不复活(latest 不得悬挂)"
    );
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// FC-MODEL-POST-005 / FC-MODEL-POST-007:同一关系数据在 `Outgoing` 与 `Both`
/// 两种索引模式下,重开后出边/入边逐边一致(反向表只加速、不改语义)。
#[test]
fn relation_index_modes_agree_after_reopen() {
    fn build(dir: &std::path::Path, index: mneme::RelationIndex) {
        let db = Builder::default()
            .dimension(2)
            .path(dir)
            .relation_index(index)
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
        ns.relate(a, b, mneme::RelationKind::SUPPORTS, 0.5)
            .expect("relate");
        db.flush().expect("flush");
        db.close().expect("close");
    }

    fn edges(path: &std::path::Path) -> (u64, u64, u64) {
        let db = Mneme::open(path).expect("reopen");
        let ns = db.namespace("demo");
        let a = ns.get("a").expect("a").expect("存在").rowid();
        let b = ns.get("b").expect("b").expect("存在").rowid();
        let out = ns
            .neighbors(a, &[mneme::RelationKind::SUPPORTS])
            .expect("out");
        let incoming = ns
            .predecessors(b, &[mneme::RelationKind::SUPPORTS])
            .expect("in");
        assert_eq!(out.len(), 1, "出边必须恢复");
        assert_eq!(incoming.len(), 1, "入边必须恢复");
        let result = (a.get(), out[0].to.get(), incoming[0].from.get());
        db.close().expect("close");
        result
    }

    let outgoing_dir = tempfile::tempdir().expect("outgoing tempdir");
    let both_dir = tempfile::tempdir().expect("both tempdir");
    build(outgoing_dir.path(), mneme::RelationIndex::Outgoing);
    build(both_dir.path(), mneme::RelationIndex::Both);
    assert_eq!(
        edges(outgoing_dir.path()),
        edges(both_dir.path()),
        "Outgoing 与 Both 的边语义必须逐边一致"
    );
}

/// FC-LIFE-POST-003 / FC-MODEL-POST-004:恢复后"槽位 → 段"归属必须回填;
/// 重开后空 flush 不产段、compaction 不得丢弃已落盘段。
#[test]
fn reopen_preserves_segment_membership() {
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
                .map(|row| Record::new(vec![row as f32, 1.0]).key(format!("k{}", batch * 4 + row)))
                .collect();
            ns.insert_batch(records).expect("batch");
            db.flush().expect("flush");
        }
        assert_eq!(db.stats().expect("stats").segments.len(), 3);
        db.close().expect("close");
    }

    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .compaction(tiered_policy())
        .build()
        .expect("reopen");
    let ns = db.namespace("demo");
    // 恢复后空 flush 必须为空操作(槽位归属已回填)。
    let before = db.stats().expect("stats").segments.len();
    db.flush().expect("flush");
    assert_eq!(
        db.stats().expect("stats").segments.len(),
        before,
        "重开后空 flush 必须为空操作(FC-LIFE-POST-003)"
    );
    // 恢复后 compaction 必须保留全部记录。
    db.compact().expect("compact");
    assert_eq!(db.stats().expect("stats").segments.len(), 2);
    for key in 0..12_u32 {
        assert!(
            ns.get(&format!("k{key}")).expect("get").is_some(),
            "重开后 compaction 不得丢记录 k{key}"
        );
    }
    db.close().expect("close");

    let db = Mneme::open(dir.path()).expect("reopen2");
    let ns = db.namespace("demo");
    for key in 0..12_u32 {
        assert!(
            ns.get(&format!("k{key}")).expect("get").is_some(),
            "再次重开后记录必须还在 k{key}"
        );
    }
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// FC-MODEL-POST-007:compaction 后旧段已删除的关系边不得经并集复活。
#[test]
fn compaction_does_not_resurrect_removed_edges() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .compaction(tiered_policy())
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
        db.flush().expect("flush base"); // 段 0:全量关系(含 a→b)
        ns.insert(Record::new(vec![1.0, 1.0]).key("c")).expect("c");
        db.flush().expect("flush c"); // 段 1
        ns.unrelate(a, b, mneme::RelationKind::RELATED)
            .expect("unrelate");
        db.flush().expect("flush unrelate"); // 段 2:delta Unrelate
        db.compact().expect("compact");
        assert_eq!(db.stats().expect("stats").segments.len(), 2);
        db.close().expect("close");
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    let a = ns.get("a").expect("a").expect("存在").rowid();
    let edges = ns
        .neighbors(a, &[mneme::RelationKind::RELATED])
        .expect("neighbors");
    assert!(
        edges.is_empty(),
        "compaction 后被删除的边不得复活,实际 {edges:?}"
    );
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// FC-PERSIST-POST-010:compaction 不得丢弃尚未随版本物化的访问增量。
#[test]
fn compaction_carries_access_delta_for_unmerged_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .compaction(tiered_policy())
            .clock(Arc::new(clock.clone()))
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        // 段 0:4 行(level 1,含 a)。
        let a = ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        let a = match a {
            mneme::InsertOutcome::Inserted(id) | mneme::InsertOutcome::Merged(id) => id,
            other => panic!("期望写入,得到 {other:?}"),
        };
        for row in 0..3_u32 {
            ns.insert(Record::new(vec![row as f32, 1.0]).key(format!("fill{row}")))
                .expect("fill");
        }
        db.flush().expect("flush level1");
        clock.set(2_000);
        assert!(ns.touch_by_rowid(a, None).expect("touch"));
        db.flush().expect("flush delta"); // 段 1:纯 delta(含 a 的 Access)
        ns.insert(Record::new(vec![9.0, 9.0]).key("tail"))
            .expect("tail");
        db.flush().expect("flush tail"); // 段 2:1 行(level 0)
        db.compact().expect("compact level0"); // 合并 [段 1, 段 2],a 的 latest 在段 0
        db.close().expect("close");
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert_eq!(
        ns.count(Some(mneme::Expr::field("access_count").eq(1)))
            .expect("count"),
        1,
        "compaction 必须携带未物化的 Access delta"
    );
    db.close().expect("close");
}

/// FC-PERSIST-POST-010:仅历史版本被合并时不得清掉该 RowId 的访问增量。
#[test]
fn compaction_keeps_access_dirty_for_history_only_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .compaction(tiered_policy())
            .clock(Arc::new(clock.clone()))
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        // 段 0(level 0):a 的 v1。
        let a = ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        let a = match a {
            mneme::InsertOutcome::Inserted(id) | mneme::InsertOutcome::Merged(id) => id,
            other => panic!("期望写入,得到 {other:?}"),
        };
        db.flush().expect("flush v1");
        // 段 1(level 1):a 的 v2(latest)+ 3 行填充。
        ns.update("a", UpdatePatch::new().importance(0.9))
            .expect("update");
        for row in 0..3_u32 {
            ns.insert(Record::new(vec![row as f32, 1.0]).key(format!("fill{row}")))
                .expect("fill");
        }
        db.flush().expect("flush v2");
        // 段 2(level 0):1 行,触发 level0 合并 [段 0, 段 2]。
        ns.insert(Record::new(vec![9.0, 9.0]).key("tail"))
            .expect("tail");
        db.flush().expect("flush tail");
        clock.set(2_000);
        assert!(ns.touch_by_rowid(a, None).expect("touch"));
        // 此时 a 的 latest(v2)在段 1、不在 level0 组内;合并后不得清 dirty。
        db.compact().expect("compact");
        db.flush().expect("flush dirty delta");
        db.close().expect("close");
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert_eq!(
        ns.count(Some(mneme::Expr::field("access_count").eq(1)))
            .expect("count"),
        1,
        "仅历史版本入段时,访问增量必须保留到下一段 delta"
    );
    db.close().expect("close");
}

/// FC-PERSIST-POST-011 / FC-LIFE-POST-006:reset 删除失败残留的旧 WAL 文件
/// 不得让已注销命名空间复活(metadata 帧同样受 watermark 约束)。
#[test]
fn stale_wal_files_do_not_resurrect_unregistered_namespace() {
    let dir = tempfile::tempdir().expect("tempdir");
    // 1. 写入注册与数据帧,不 close(仅 drop):WAL 保留完整帧。
    {
        let db = build(dir.path(), 2);
        db.namespace("demo")
            .insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("a");
    }
    // 2. 备份这份"旧 WAL"(模拟 reset 删除失败残留的文件)。
    let wal_path = dir.path().join("wal").join("wal_000001.log");
    let stale = std::fs::read(&wal_path).expect("read stale wal");

    // 3. 重开、注销并 flush:注册表清空、watermark 覆盖旧帧,reset 删除旧文件。
    {
        let db = Mneme::open(dir.path()).expect("reopen");
        assert_eq!(db.drop_namespace("demo").expect("drop"), 1);
        db.flush().expect("flush");
        db.close().expect("close");
    }

    // 4. 把旧 WAL 文件放回。
    std::fs::write(&wal_path, &stale).expect("restore stale wal");

    // 5. 重开:旧帧(含 NsRegister)必须被水位跳过,注销不得复活。
    let db = Mneme::open(dir.path()).expect("reopen2");
    assert!(
        db.list_namespaces().expect("list").is_empty(),
        "残留旧 WAL 不得复活已注销命名空间"
    );
    assert!(db.namespace("demo").get("a").expect("get").is_none());
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// FC-PERSIST-ERR-006:损坏段在非 fail-fast 下只在内存跳过、文件保持原地;
/// 再次打开不得因"引用段缺失"而拒绝启动。
#[test]
fn skipped_corrupt_segment_keeps_library_openable() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        let ns = db.namespace("demo");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        db.flush().expect("flush a");
        ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
        db.flush().expect("flush b");
        db.close().expect("close");
    }
    // 破坏段 0 的 vsec 魔数。
    let seg0 = dir.path().join("segments").join("seg_000000.vsec");
    let mut bytes = std::fs::read(&seg0).expect("read seg0");
    bytes[0] ^= 0xFF;
    std::fs::write(&seg0, &bytes).expect("corrupt seg0");
    assert!(seg0.exists());

    // 第一次打开:内存跳过损坏段,其余数据可读;损坏段文件绝不被移走/删除。
    {
        let db = Mneme::open(dir.path()).expect("first open");
        assert!(db.namespace("demo").get("b").expect("get b").is_some());
        db.close().expect("close");
    }
    assert!(seg0.exists(), "损坏段文件必须保持原地");

    // 第二次打开:仍成功(不会因 MANIFEST 引用缺失而拒启)。
    let db = Mneme::open(dir.path()).expect("second open");
    assert!(db.namespace("demo").get("b").expect("get b").is_some());
    db.close().expect("close");
}

/// FC-PERSIST-POST-010:最新版本随合并段物化时,其已被版本行覆盖的 `Access`
/// delta 不得再携带(否则恢复期重复累加)。
#[test]
fn compaction_latest_in_keep_drops_covered_delta() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock = FakeClock::default();
    clock.set(1_000);
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .compaction(tiered_policy())
            .clock(Arc::new(clock.clone()))
            .build()
            .expect("build");
        let ns = db.namespace("demo");
        let a = ns.insert(Record::new(vec![1.0, 0.0]).key("a")).expect("a");
        let a = match a {
            mneme::InsertOutcome::Inserted(id) | mneme::InsertOutcome::Merged(id) => id,
            other => panic!("期望写入,得到 {other:?}"),
        };
        db.flush().expect("flush a"); // 段 0(level 0,含 a)
        clock.set(2_000);
        assert!(ns.touch_by_rowid(a, None).expect("touch"));
        ns.insert(Record::new(vec![0.0, 1.0]).key("b")).expect("b");
        db.flush().expect("flush b"); // 段 1(level 0 + a 的 Access delta)
        // 合并 [段 0, 段 1]:a 的最新版本随新段物化,其 delta 已被版本行覆盖。
        db.compact().expect("compact level0");
        db.close().expect("close");
    }

    let db = Mneme::open(dir.path()).expect("reopen");
    let ns = db.namespace("demo");
    assert_eq!(
        ns.count(Some(mneme::Expr::field("access_count").eq(1)))
            .expect("count"),
        1,
        "最新版本入段时不得重复携带已被覆盖的 Access delta"
    );
    assert_eq!(
        ns.count(Some(mneme::Expr::field("access_count").ge(2)))
            .expect("count"),
        0
    );
    db.close().expect("close");
}

/// FC-LIFE-POST-006 / FC-PERSIST-POST-011:上次 flush 之后新注册的命名空间在
/// 崩溃重开后仍可见(注册/注销 metadata 帧必须有真实 seqno 并参与水位)。
#[test]
fn namespace_registered_after_last_flush_survives_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let db = build(dir.path(), 2);
        db.namespace("a")
            .insert(Record::new(vec![1.0, 0.0]).key("x"))
            .expect("x");
        db.flush().expect("flush a"); // watermark > 0
        // flush 之后注册的新命名空间,未再 flush。
        db.namespace("b")
            .insert(Record::new(vec![0.0, 1.0]).key("y"))
            .expect("y");
    } // 不 close:模拟崩溃

    let db = Mneme::open(dir.path()).expect("reopen");
    assert!(
        db.list_namespaces()
            .expect("list")
            .contains(&"b".to_string()),
        "flush 之后的注册不得因水位判定丢失"
    );
    assert!(db.namespace("b").get("y").expect("get y").is_some());
    db.close().expect("close");
}
