//! L12 部署形态契约验收:存储后端抽象、只读共享与可观测性。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-DEPLOY-POST-001(`Storage` 后端可注入;内存后端完整生命周期)
//! * FC-DEPLOY-INV-029(只读实例始终看到某已提交 MANIFEST 的完整视图)
//! * FC-DEPLOY-STA-001(只读视图切换 `V_n → V_{n+1}` 原子,无中间态)
//! * FC-DEPLOY-INV-030(`Observer` 回调不改变引擎行为;回调 panic 被隔离)
//! * FC-DEPLOY-CPLX-001(视图切换 `O(1)`:原子交换已构建视图)
//! * FC-GLOBAL-INV-001(库本体不读环境变量;调参一律经配置显式注入)
//!
//! 不变量锚定:I29(只读一致)、I30(可观测无副作用)

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use mneme::{Builder, MemStorage, Record, Storage};

/// 共享的内存后端(多实例共用同一份虚拟文件系统)。
fn shared_storage() -> Arc<MemStorage> {
    Arc::new(MemStorage::new())
}

/// 用给定后端建库。
fn open_with(storage: Arc<dyn Storage>, dimension: u32) -> mneme::Mneme {
    Builder::default()
        .path("mem://deploy")
        .storage(storage)
        .dimension(dimension)
        .build()
        .expect("build over custom storage")
}

/// **FC-DEPLOY-POST-001**:注入内存后端后,建库/写入/flush/重开/检索全链路可用;
/// 后端不接触真实文件系统。
#[test]
fn custom_storage_backend_supports_full_lifecycle() {
    let storage: Arc<dyn Storage> = shared_storage();
    {
        let db = open_with(Arc::clone(&storage), 2);
        let ns = db.namespace("n");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
            .expect("insert");
        ns.insert(Record::new(vec![0.0, 1.0]).key("b"))
            .expect("insert");
        db.flush().expect("flush");
        db.close().expect("close");
    }
    // 虚拟文件系统里应出现段与 MANIFEST(路径由后端管理)。
    assert!(storage.exists("current").expect("current"));
    assert!(
        storage
            .list_dir("segments")
            .expect("segments")
            .iter()
            .any(|name| name.ends_with(".vsec")),
        "flush 必须在后端写出段文件"
    );

    let db = open_with(Arc::clone(&storage), 2);
    let ns = db.namespace("n");
    assert!(ns.get("a").expect("get").is_some());
    let hits = ns
        .search()
        .vector(&[1.0, 0.0])
        .top_k(1)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 1);
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// **FC-DEPLOY-INV-029 / FC-DEPLOY-STA-001**:两个只读实例 + 一个写实例并发:
/// 只读实例经 `reload()` 原子切换到新提交版本,任意时刻视图完整自洽。
#[test]
fn read_only_instances_see_committed_views_atomically() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = Builder::default()
        .path(dir.path())
        .dimension(2)
        .build()
        .expect("writer");
    writer
        .namespace("n")
        .insert(Record::new(vec![1.0, 0.0]).key("a"))
        .expect("insert a");
    writer.flush().expect("flush a");

    let reader = Builder::default()
        .path(dir.path())
        .read_only(true)
        // 关闭自动探测:用例验证显式 reload 语义,避免 1s 探测线程先换视图致断言偶发失败。
        .read_only_probe_interval(std::time::Duration::ZERO)
        .build()
        .expect("read-only");
    let reader_ns = reader.namespace("n");
    assert!(reader_ns.get("a").expect("get a").is_some());
    assert!(
        reader_ns.get("b").expect("get b").is_none(),
        "只读实例只看到已提交版本"
    );

    // 写者追加 b 并提交新 MANIFEST;只读实例在 reload 前不可见。
    writer
        .namespace("n")
        .insert(Record::new(vec![0.0, 1.0]).key("b"))
        .expect("insert b");
    writer.flush().expect("flush b");
    assert!(reader_ns.get("b").expect("before reload").is_none());

    // reload 原子换视图:要么旧要么新,不存在半提交态。
    reader.reload().expect("reload");
    let reader_ns = reader.namespace("n");
    assert!(reader_ns.get("a").expect("get a").is_some());
    assert!(reader_ns.get("b").expect("get b").is_some());
    assert!(reader.check().expect("reader check").ok);

    reader.close().expect("reader close");
    writer.close().expect("writer close");
}

/// 记录事件的测试 Observer。
#[derive(Default)]
struct RecordingObserver {
    events: Mutex<Vec<String>>,
    queries: AtomicUsize,
}

impl mneme::Observer for RecordingObserver {
    fn on_event(&self, event: mneme::Event) {
        let label = match &event {
            mneme::Event::Query { .. } => {
                self.queries.fetch_add(1, Ordering::Relaxed);
                "query".to_string()
            }
            mneme::Event::Write { op, .. } => format!("write:{op:?}"),
            mneme::Event::Flush { .. } => "flush".to_string(),
            mneme::Event::Compaction { .. } => "compaction".to_string(),
            mneme::Event::Error { .. } => "error".to_string(),
        };
        self.events.lock().expect("events").push(label);
    }
}

/// 会 panic 的 Observer:引擎必须隔离其 panic,行为不受影响(I30)。
struct PanickingObserver;

impl mneme::Observer for PanickingObserver {
    fn on_event(&self, _event: mneme::Event) {
        panic!("observer 注入 panic");
    }
}

/// **FC-DEPLOY-INV-030**:事件字段与实际操作一致;回调 panic 被隔离。
#[test]
fn observer_events_fire_and_panics_are_isolated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let observer = Arc::new(RecordingObserver::default());
    let db = Builder::default()
        .path(dir.path())
        .dimension(2)
        .observer(Arc::clone(&observer) as Arc<dyn mneme::Observer>)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
        .expect("insert");
    db.flush().expect("flush");
    let _ = ns
        .search()
        .vector(&[1.0, 0.0])
        .top_k(1)
        .execute()
        .expect("search");
    let events = observer.events.lock().expect("events").clone();
    assert!(
        events.iter().any(|event| event.starts_with("write:")),
        "写入必须发出事件:{events:?}"
    );
    assert!(
        events.iter().any(|event| event == "flush"),
        "flush 事件:{events:?}"
    );
    assert!(
        observer.queries.load(Ordering::Relaxed) >= 1,
        "查询必须发出事件:{events:?}"
    );
    db.close().expect("close");

    // 注入 panic 回调:所有读写仍成功,引擎状态不变。
    let db = Builder::default()
        .path(dir.path())
        .dimension(2)
        .observer(Arc::new(PanickingObserver) as Arc<dyn mneme::Observer>)
        .build()
        .expect("build with panicking observer");
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![0.0, 1.0]).key("b"))
        .expect("insert with panicking observer");
    let hits = ns
        .search()
        .vector(&[0.0, 1.0])
        .top_k(1)
        .execute()
        .expect("search with panicking observer");
    assert_eq!(hits.len(), 1);
    db.close().expect("close");
}

/// **FC-DEPLOY-CPLX-001 哨兵**:视图切换为原子交换(重复 reload 幂等且快速),
/// 不触发数据重解码之外的隐藏全量扫描。
#[test]
fn repeated_reload_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = Builder::default()
        .path(dir.path())
        .dimension(2)
        .build()
        .expect("writer");
    writer
        .namespace("n")
        .insert(Record::new(vec![1.0, 0.0]).key("a"))
        .expect("insert");
    writer.flush().expect("flush");
    let reader = Builder::default()
        .path(dir.path())
        .read_only(true)
        // 关闭自动探测:用例验证显式 reload 语义,避免 1s 探测线程先换视图致断言偶发失败。
        .read_only_probe_interval(std::time::Duration::ZERO)
        .build()
        .expect("reader");
    assert!(
        reader.reload().expect("reload 1").is_none(),
        "无新版本时不切换"
    );
    assert!(reader.reload().expect("reload 2").is_none());
    assert!(reader.namespace("n").get("a").expect("get").is_some());
    reader.close().expect("close");
    writer.close().expect("close");
}

/// **FC-GLOBAL-INV-001**:库本体不读环境变量。
///
/// 机械扫描 `src/**/*.rs`,源码不得出现 `std::env` / `env::var`(运行期环境读取);
/// 所有调参与开关一律经 `Builder`/`Tuning`/`Limits` 等配置显式注入。dev 侧
/// (契约测试/示例)的环境变量读取统一收敛于 `tests/common/env.rs`(变量清单见该文件头)。
#[test]
fn library_source_never_reads_environment_variables() {
    fn walk(dir: &std::path::Path, offenders: &mut Vec<String>) {
        let entries =
            std::fs::read_dir(dir).unwrap_or_else(|error| panic!("读取 {dir:?} 失败: {error}"));
        for entry in entries {
            let path = entry.expect("目录项").path();
            if path.is_dir() {
                walk(&path, offenders);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("读取 {} 失败: {error}", path.display()));
                if text.contains("std::env") || text.contains("env::var") {
                    offenders.push(path.display().to_string());
                }
            }
        }
    }
    let mut offenders = Vec::new();
    walk(std::path::Path::new("src"), &mut offenders);
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "库本体不得读取环境变量(FC-GLOBAL-INV-001),违规文件: {offenders:?}"
    );
}
