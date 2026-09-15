//! L6 打磨层契约验收:量化副本、两阶段检索、自动回退与 async 门面。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-QUANT-PRE-001(量化配置校验与纯内存限制)
//! * FC-QUANT-POST-002(qvec 段格式往返/重开)
//! * FC-QUANT-POST-003(compaction 按当前配置重写副本)
//! * FC-QUANT-POST-004(量化两阶段召回损失 ≤ 2%)
//! * FC-QUANT-INV-012(I12:`Hit.score` = f32 精排分)
//! * FC-QUANT-INV-013(I13:抽样不达标自动回退、`stats()` 可见)
//! * FC-QUANT-INV-014(I14:async 与 sync 等价)
//! * FC-QUANT-ERR-001(f16 feature 门控)
//! * FC-QUANT-ERR-002(纯内存库配置量化 → `Unsupported`)
//! * FC-QUANT-ERR-003(qvec 区损坏可检出)
//!
//! 不变量锚定:I12(量化 `Hit.score` = f32 精排分)、I13(召回不达标自动回退)、
//! I14(async 与 sync 等价)

use std::collections::HashMap;
use std::path::Path;

#[cfg(feature = "async")]
use mneme::{AsyncNamespace, Namespace, UpdatePatch};
use mneme::{
    Builder, CompactionPolicy, Hit, Metric, Mneme, MnemeError, Record, Tuning, VectorFormat,
};

mod common;

/// 测试维度(16 维 = 64B 行步长,恰 32B 对齐;行补齐分支由
/// `src/persist/vsec/tests.rs` 的 4 维单测覆盖)。
const DIM: u32 = 16;

/// 强制 ANN + 关闭自动回退的竖控参数(抽样门槛 0 恒通过)。
fn ann_tuning() -> Tuning {
    Tuning {
        brute_force_max_rows: 8,
        quant_recall_floor: 0.0,
        ..Tuning::default()
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

/// 确定性伪随机向量(线性同余,避免依赖 `rand`)。
fn vector(seed: u64, dim: usize) -> Vec<f32> {
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (0..dim)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f32 / (1_u64 << 31) as f32) - 0.5
        })
        .collect()
}

/// 在临时目录新建指定量化格式的持久库。
fn build(dir: &Path, format: VectorFormat, tuning: Tuning) -> Mneme {
    Builder::default()
        .path(dir)
        .dimension(DIM)
        .metric(Metric::Dot)
        .quantization(format)
        .tuning(tuning)
        .build()
        .expect("build")
}

/// 生成 `rows` 条确定性记录。
fn records(rows: usize) -> Vec<Record> {
    (0..rows)
        .map(|row| Record::new(vector(row as u64 + 1, DIM as usize)).key(format!("k{row}")))
        .collect()
}

/// 暴力点积参考 top-k(键无关,返回 `RowId`)。
fn brute_top(ns: &mneme::Namespace, query: &[f32], k: usize) -> Vec<u64> {
    let stored: Vec<(u64, Vec<f32>)> = ns
        .iter(None)
        .expect("iter")
        .map(|record| {
            let record = record.expect("record");
            (record.rowid().get(), record.vector().to_vec())
        })
        .collect();
    common::reference_dot(query, &stored, k)
}

/// 两个 `RowId` 集合的交集大小。
fn overlap(left: &[u64], right: &[u64]) -> usize {
    left.iter().filter(|id| right.contains(id)).count()
}

/// 在 `n` 命名空间上执行确定性 top-k 查询。
fn top_k(db: &Mneme, query: &[f32], k: usize) -> Vec<Hit> {
    db.namespace("n")
        .search()
        .vector(query)
        .top_k(k)
        .ef(64)
        .execute()
        .expect("search")
}

/// 建库、写入给定记录并 flush。
fn flushed_db_with(dir: &Path, format: VectorFormat, batch: &[Record]) -> Mneme {
    let db = build(dir, format, ann_tuning());
    db.namespace("n")
        .insert_batch(batch.to_vec())
        .expect("insert_batch");
    db.flush().expect("flush");
    db
}

/// 建库、写入 `rows` 条确定性记录并 flush。
fn flushed_db(dir: &Path, format: VectorFormat, rows: usize) -> Mneme {
    flushed_db_with(dir, format, &records(rows))
}

/// 取出建库错误(`Mneme` 未实现 `Debug`,`expect_err` 不适用)。
fn build_error(result: mneme::Result<Mneme>, context: &str) -> MnemeError {
    match result {
        Ok(_) => panic!("{context}"),
        Err(error) => error,
    }
}

/// FC-QUANT-PRE-001 / FC-QUANT-ERR-002:纯内存库没有段,配置量化在构造期返回
/// `Unsupported`,绝不静默记录配置;`F32` 内存库照常可建。
#[test]
fn in_memory_quantization_is_unsupported() {
    let error = build_error(
        Builder::default()
            .dimension(DIM)
            .quantization(VectorFormat::I8Rescored)
            .build(),
        "纯内存库配 i8 量化必须拒绝",
    );
    assert!(
        matches!(error, MnemeError::Unsupported { .. }),
        "got {error:?}"
    );
    assert!(Mneme::in_memory(DIM).is_ok());
}

/// FC-QUANT-PRE-001:`rescore_oversample = 0` 与 `quant_recall_floor = NaN`
/// 在构造期返回 `Config`(拒绝静默空转/恒回退)。
#[test]
fn invalid_tuning_rejects_quantization_knobs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let zero = build_error(
        Builder::default()
            .path(dir.path())
            .dimension(DIM)
            .tuning(Tuning {
                rescore_oversample: 0,
                ..Tuning::default()
            })
            .build(),
        "rescore_oversample=0 必须拒绝",
    );
    assert!(matches!(zero, MnemeError::Config { .. }), "got {zero:?}");

    let nan = build_error(
        Builder::default()
            .path(dir.path())
            .dimension(DIM)
            .tuning(Tuning {
                quant_recall_floor: f32::NAN,
                ..Tuning::default()
            })
            .build(),
        "quant_recall_floor=NaN 必须拒绝",
    );
    assert!(matches!(nan, MnemeError::Config { .. }), "got {nan:?}");
}

/// FC-QUANT-INV-012:量化模式下每条 `Hit.score` 都等于对同一记录用 f32 原向量
/// 重算的精确分;与纯 f32 库同 key 分数逐位一致。
#[test]
fn quantized_hit_score_matches_f32_exact() {
    let quant_dir = tempfile::tempdir().expect("tempdir");
    let exact_dir = tempfile::tempdir().expect("tempdir");
    let quant = flushed_db(quant_dir.path(), VectorFormat::I8Rescored, 128);
    let exact = flushed_db(exact_dir.path(), VectorFormat::F32, 128);

    let stats = quant.stats().expect("stats");
    assert_eq!(stats.quant.configured, VectorFormat::I8Rescored);
    assert_eq!(stats.quant.active, VectorFormat::I8Rescored);
    assert!(stats.quant.recall_est.is_some(), "建段会话应给出召回估计");

    let query = vector(9_999, DIM as usize);
    let quant_hits = top_k(&quant, &query, 10);
    let exact_hits = top_k(&exact, &query, 10);
    assert!(!quant_hits.is_empty());
    let exact_scores: HashMap<u64, f32> = exact_hits
        .iter()
        .map(|hit| (hit.rowid.get(), hit.score))
        .collect();

    let ns = quant.namespace("n");
    for hit in &quant_hits {
        let stored = ns
            .get_by_rowid(hit.rowid)
            .expect("get_by_rowid")
            .expect("visible");
        let expected = Metric::Dot.score(&query, stored.vector(), 0.0, 0.0);
        assert_eq!(hit.score, expected, "命中分必须等于 f32 精排分");
        assert_eq!(
            exact_scores.get(&hit.rowid.get()),
            Some(&hit.score),
            "两库同记录分数必须逐位一致"
        );
    }
}

/// FC-QUANT-INV-013:抽样门槛不可达(`quant_recall_floor = 2.0`)时该段自动回退
/// f32:不写 qvec,`stats()` 如实显示 `configured = I8Rescored / active = F32`。
#[test]
fn unreachable_recall_floor_falls_back_to_f32() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = build(
        dir.path(),
        VectorFormat::I8Rescored,
        Tuning {
            brute_force_max_rows: 8,
            quant_recall_floor: 2.0,
            ..Tuning::default()
        },
    );
    db.namespace("n").insert_batch(records(64)).expect("insert");
    db.flush().expect("flush");

    let stats = db.stats().expect("stats");
    assert_eq!(stats.quant.configured, VectorFormat::I8Rescored);
    assert_eq!(stats.quant.active, VectorFormat::F32, "不达标必须回退");
    assert!(stats.quant.recall_est.is_none());
    assert!(
        stats
            .segments
            .iter()
            .all(|segment| segment.quant == VectorFormat::F32),
        "回退段不得携带量化副本"
    );
    // 回退后检索仍可用(I12 精排口径不变)。
    let hits = db
        .namespace("n")
        .search()
        .vector(&vector(3, DIM as usize))
        .top_k(4)
        .ef(64)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 4);
    assert!(db.check().expect("check").ok);
}

/// FC-QUANT-INV-013:门槛为 0(关闭回退)时量化生效,`active`/`recall_est` 与
/// 每段统计一致,且召回估计落在 `[0, 1]`。
#[test]
fn quantized_segment_reports_recall_estimate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = build(dir.path(), VectorFormat::I8Rescored, ann_tuning());
    db.namespace("n").insert_batch(records(96)).expect("insert");
    db.flush().expect("flush");

    let stats = db.stats().expect("stats");
    assert_eq!(stats.quant.active, VectorFormat::I8Rescored);
    let estimate = stats.quant.recall_est.expect("应给出召回估计");
    assert!((0.0..=1.0).contains(&estimate), "估计越界:{estimate}");
    for segment in &stats.segments {
        assert_eq!(segment.quant, VectorFormat::I8Rescored);
        assert!(segment.recall_est.is_some());
    }
}

/// FC-QUANT-POST-002:i8 qvec 随段落盘,重开后副本/格式/召回口径完整保留,
/// f32 原向量逐位不变(精排基准)。
#[test]
fn i8_qvec_roundtrip_after_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let original: Vec<Vec<f32>> = (0..48)
        .map(|row| vector(row as u64 + 1, DIM as usize))
        .collect();
    {
        let db = build(dir.path(), VectorFormat::I8Rescored, ann_tuning());
        db.namespace("n").insert_batch(records(48)).expect("insert");
        db.flush().expect("flush");
    }
    let db = build(dir.path(), VectorFormat::I8Rescored, ann_tuning());
    let stats = db.stats().expect("stats");
    assert_eq!(
        stats.quant.active,
        VectorFormat::I8Rescored,
        "重开后量化仍生效"
    );
    assert!(
        stats.quant.recall_est.is_none(),
        "重开后无建段采样上下文,recall_est 必须为 None(设计 16 §1.6)"
    );
    let ns = db.namespace("n");
    for (row, expected) in original.iter().enumerate() {
        let record = ns.get(&format!("k{row}")).expect("get").expect("visible");
        assert_eq!(
            record.vector(),
            expected.as_slice(),
            "f32 原向量必须逐位保留"
        );
    }
    let hits = ns
        .search()
        .vector(&vector(5, DIM as usize))
        .top_k(4)
        .ef(64)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 4);
    assert!(db.check().expect("check").ok);
}

/// FC-QUANT-ERR-003:qvec 区被篡改后 payload CRC 校验必须失败,`fail_fast` 下
/// 打开返回 `Corrupted`,绝不部分解析。
#[test]
fn qvec_corruption_is_detected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let segment_id = {
        let db = build(dir.path(), VectorFormat::I8Rescored, ann_tuning());
        db.namespace("n").insert_batch(records(32)).expect("insert");
        db.flush().expect("flush");
        db.stats().expect("stats").segments[0].id.get()
    };
    let path = dir
        .path()
        .join("segments")
        .join(format!("seg_{segment_id:06}.vsec"));
    let mut bytes = std::fs::read(&path).expect("read segment");
    // vec 区(每行 32B 对齐)与 norm 区之后即 i8 参数表;翻转一个参数字节。
    let stride = (DIM as usize * 4).div_ceil(32) * 32;
    let params_offset = 64 + 32 * stride + 32 * 4;
    bytes[params_offset] ^= 0xFF;
    std::fs::write(&path, &bytes).expect("write segment");

    let error = build_error(
        Builder::default()
            .path(dir.path())
            .dimension(DIM)
            .metric(Metric::Dot)
            .quantization(VectorFormat::I8Rescored)
            .verify_on_open(true)
            .fail_fast_on_corruption(true)
            .build(),
        "损坏段必须拒绝打开",
    );
    assert!(
        matches!(error, MnemeError::Corrupted { .. }),
        "got {error:?}"
    );
}

/// FC-QUANT-POST-003:compaction 按**当前**配置重写副本——f32 旧段合并后
/// 新段带 i8 副本,格式迁移零特殊逻辑。
#[test]
fn compaction_rewrites_quantized_copies() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        // 先以 f32 建两段。
        let db = Builder::default()
            .path(dir.path())
            .dimension(DIM)
            .metric(Metric::Dot)
            .compaction(tiered_policy())
            .build()
            .expect("build");
        let ns = db.namespace("n");
        ns.insert_batch(records(4)[..4].to_vec()).expect("batch 1");
        db.flush().expect("flush 1");
        ns.insert_batch(records(8)[4..8].to_vec()).expect("batch 2");
        db.flush().expect("flush 2");
        assert_eq!(db.stats().expect("stats").segments.len(), 2);
    }
    // 以 i8 配置重开并合并:旧 f32 段被按当前配置重写。
    let db = Builder::default()
        .path(dir.path())
        .dimension(DIM)
        .metric(Metric::Dot)
        .quantization(VectorFormat::I8Rescored)
        .tuning(ann_tuning())
        .compaction(tiered_policy())
        .build()
        .expect("reopen");
    db.compact().expect("compact");
    let stats = db.stats().expect("stats");
    assert_eq!(stats.segments.len(), 1, "同层两段应合并为一段");
    assert_eq!(stats.quant.active, VectorFormat::I8Rescored);
    assert_eq!(stats.segments[0].quant, VectorFormat::I8Rescored);
}

/// 48 个簇、每簇 8 条近邻的确定性数据。
fn clustered_records() -> Vec<Record> {
    let mut records: Vec<Record> = Vec::new();
    for cluster in 0..48_u64 {
        let center = vector(1_000 + cluster, DIM as usize);
        for member in 0..8_u64 {
            let noise = vector(50_000 + cluster * 8 + member, DIM as usize);
            let point: Vec<f32> = center
                .iter()
                .zip(&noise)
                .map(|(base, jitter)| base + jitter * 0.01)
                .collect();
            records.push(Record::new(point).key(format!("c{cluster}_m{member}")));
        }
    }
    records
}

/// 取前 10 个簇心附近的确定性查询。
fn cluster_queries() -> Vec<Vec<f32>> {
    (0..10_u64)
        .map(|cluster| {
            let center = vector(1_000 + cluster, DIM as usize);
            let noise = vector(90_000 + cluster, DIM as usize);
            center
                .iter()
                .zip(&noise)
                .map(|(base, jitter)| base + jitter * 0.005)
                .collect()
        })
        .collect()
}

/// 单查询的量化 Recall@10(相对暴力参考)。
fn recall_at_10(db: &Mneme, query: &[f32]) -> f64 {
    let reference = brute_top(&db.namespace("n"), query, 10);
    let hits: Vec<u64> = top_k(db, query, 10)
        .iter()
        .map(|hit| hit.rowid.get())
        .collect();
    overlap(&reference, &hits) as f64 / 10.0
}

/// FC-QUANT-POST-004:小规模确定性数据上,量化两阶段的 Recall@10 相对 f32 的
/// Recall@10 损失不超过 2%(离线 1M 基准另见设计 14 §4)。
#[test]
fn quantized_two_stage_recall_loss_within_two_percent() {
    let records = clustered_records();
    let queries = cluster_queries();

    let exact_dir = tempfile::tempdir().expect("tempdir");
    let quant_dir = tempfile::tempdir().expect("tempdir");
    let exact = flushed_db_with(exact_dir.path(), VectorFormat::F32, &records);
    let quant = flushed_db_with(quant_dir.path(), VectorFormat::I8Rescored, &records);
    assert_eq!(
        quant.stats().expect("stats").quant.active,
        VectorFormat::I8Rescored
    );

    let mean = |db: &Mneme| {
        queries
            .iter()
            .map(|query| recall_at_10(db, query))
            .sum::<f64>()
            / queries.len() as f64
    };
    let recall_exact = mean(&exact);
    let recall_quant = mean(&quant);
    assert!(
        recall_quant + 0.02 >= recall_exact,
        "量化召回损失超 2%:quant={recall_quant}, exact={recall_exact}"
    );
}

/// FC-QUANT-ERR-001:未开 `quant-f16` 时构造期拒绝 `F16`,绝不静默降级。
#[test]
#[cfg(not(feature = "quant-f16"))]
fn f16_requires_feature_at_build() {
    let dir = tempfile::tempdir().expect("tempdir");
    let error = build_error(
        Builder::default()
            .path(dir.path())
            .dimension(DIM)
            .quantization(VectorFormat::F16)
            .build(),
        "未开 feature 时 F16 必须拒绝",
    );
    assert!(
        matches!(
            error,
            MnemeError::Unsupported {
                feature: "quant-f16"
            }
        ),
        "got {error:?}"
    );
}

/// FC-QUANT-ERR-001(正例):开启 `quant-f16` 后 f16 副本可建、可重开、可检索,
/// `Hit.score` 仍为 f32 精排分。
#[test]
#[cfg(feature = "quant-f16")]
fn f16_roundtrip_when_feature_enabled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rows = records(64);
    {
        let db = build(dir.path(), VectorFormat::F16, ann_tuning());
        db.namespace("n").insert_batch(rows).expect("insert");
        db.flush().expect("flush");
        assert_eq!(db.stats().expect("stats").quant.active, VectorFormat::F16);
    }
    let db = build(dir.path(), VectorFormat::F16, ann_tuning());
    assert_eq!(
        db.stats().expect("stats").quant.active,
        VectorFormat::F16,
        "重开后 f16 副本必须仍生效"
    );
    let query = vector(7, DIM as usize);
    let hits = db
        .namespace("n")
        .search()
        .vector(&query)
        .top_k(4)
        .ef(64)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 4);
    let ns = db.namespace("n");
    for hit in &hits {
        let stored = ns
            .get_by_rowid(hit.rowid)
            .expect("get_by_rowid")
            .expect("visible");
        let expected = Metric::Dot.score(&query, stored.vector(), 0.0, 0.0);
        assert_eq!(hit.score, expected);
    }
    assert!(db.check().expect("check").ok);
}

/// async 契约测试共用的种子记录。
#[cfg(feature = "async")]
fn seed(key: &str, text: &str) -> Record {
    Record::new(vector(key.len() as u64 + 1, DIM as usize))
        .key(key)
        .text(text)
}

/// 同步操作序列(插入 / 更新 / 删除 / 触碰)。
#[cfg(feature = "async")]
fn run_sync_sequence(ns: &Namespace) {
    ns.insert(seed("a", "one")).expect("insert a");
    ns.insert(seed("b", "two")).expect("insert b");
    ns.insert(seed("c", "three")).expect("insert c");
    ns.update("a", UpdatePatch::new().text(Some("uno".to_string())))
        .expect("update a");
    ns.delete("b").expect("delete b");
    ns.touch("c", None).expect("touch c");
}

/// 与 [`run_sync_sequence`] 等价的异步序列(逐步 await)。
#[cfg(feature = "async")]
async fn run_async_sequence(ns: &AsyncNamespace) {
    ns.insert(seed("a", "one")).await.expect("insert a");
    ns.insert(seed("b", "two")).await.expect("insert b");
    ns.insert(seed("c", "three")).await.expect("insert c");
    ns.update("a", UpdatePatch::new().text(Some("uno".to_string())))
        .await
        .expect("update a");
    ns.delete("b").await.expect("delete b");
    ns.touch("c", None).await.expect("touch c");
}

/// 逐 key 断言同步/异步两个命名空间的状态与存在性一致。
#[cfg(feature = "async")]
fn assert_namespaces_equivalent(
    sync_ns: &Namespace,
    async_ns: &AsyncNamespace,
    runtime: &tokio::runtime::Runtime,
) {
    assert_eq!(
        sync_ns.count(None).expect("count"),
        runtime.block_on(async_ns.count(None)).expect("count")
    );
    for key in ["a", "b", "c"] {
        let left = sync_ns.get(key).expect("get").map(|record| {
            (
                record.key().map(str::to_owned),
                record.text().map(str::to_owned),
            )
        });
        let right = runtime
            .block_on(async_ns.get(key))
            .expect("get")
            .map(|record| {
                (
                    record.key().map(str::to_owned),
                    record.text().map(str::to_owned),
                )
            });
        assert_eq!(left, right, "key {key} 状态不一致");
        assert_eq!(
            sync_ns.exists(key).expect("exists"),
            runtime.block_on(async_ns.exists(key)).expect("exists")
        );
    }
}

/// FC-QUANT-INV-014:同一操作序列下 async 门面与同步 API 产生等价状态
/// (无部分写入、返回结果一致),且错误语义一致。
#[test]
#[cfg(feature = "async")]
fn async_and_sync_namespace_sequences_are_equivalent() {
    let sync_db = Mneme::in_memory(DIM).expect("sync db");
    let async_db = Mneme::in_memory(DIM).expect("async db");
    let sync_ns = sync_db.namespace("n");
    let async_ns = async_db.namespace("n").into_async();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");

    run_sync_sequence(&sync_ns);
    runtime.block_on(run_async_sequence(&async_ns));
    assert_namespaces_equivalent(&sync_ns, &async_ns, &runtime);

    // 错误语义一致:重复 key 在默认去重策略下产生同类结果。
    let sync_dup = sync_ns.insert(seed("a", "again"));
    let async_dup = runtime.block_on(async_ns.insert(seed("a", "again")));
    assert_eq!(
        format!("{:?}", sync_dup),
        format!("{:?}", async_dup),
        "重复写入的返回语义必须一致"
    );
}

/// FC-QUANT-ERR-002 跨 feature 第 1 阶段(CI,`--features quant-f16`):
/// 在 `MNEME_F16_FIXTURE` 指定目录建一个含 f16 段的持久库,供默认构建打开断言。
#[cfg(feature = "quant-f16")]
#[test]
#[ignore = "CI 跨 feature 矩阵 phase 1:先用 --features quant-f16 建库"]
fn write_f16_fixture_for_cross_feature_check() {
    let dir = common::env::require(common::env::F16_FIXTURE);
    let path = Path::new(&dir);
    std::fs::create_dir_all(path).expect("创建 fixture 目录");
    let db = Builder::default()
        .dimension(DIM)
        .path(path)
        .quantization(VectorFormat::F16)
        .build()
        .expect("建库");
    db.namespace("f16")
        .insert(Record::new(vec![1.0_f32; DIM as usize]).key("a"))
        .expect("写入");
    db.flush().expect("flush");
    db.close().expect("close");
    assert!(
        path.join("segments").join("seg_000000.vsec").exists(),
        "fixture 必须写出段文件"
    );
}

/// FC-QUANT-ERR-002 跨 feature 第 2 阶段(CI,默认构建):打开含 f16 段的库
/// 必须返回 `Unsupported`,绝不静默按 f32 服务。
#[cfg(not(feature = "quant-f16"))]
#[test]
#[ignore = "CI 跨 feature 矩阵 phase 2:默认构建打开 f16 库必须拒绝"]
fn open_f16_fixture_requires_feature() {
    let dir = common::env::require(common::env::F16_FIXTURE);
    let error = Mneme::open(Path::new(&dir))
        .err()
        .expect("未开 quant-f16 的构建必须拒绝 f16 段");
    assert!(
        matches!(
            error,
            MnemeError::Unsupported {
                feature: "quant-f16"
            }
        ),
        "必须返回 Unsupported {{ feature: quant-f16 }},实际:{error:?}"
    );
}
