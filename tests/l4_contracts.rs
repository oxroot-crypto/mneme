//! L4 检索层契约验收测试。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-QUERY-ERR-001(DSL 任意输入不 panic、数字/时间越界结构化拒绝)
//! * FC-QUERY-POST-002(Display / JSON 往返)
//! * FC-QUERY-POST-003(BM25 公式行为、精确分数)
//! * FC-QUERY-POST-004(融合与参数校验)
//! * FC-QUERY-POST-005(过滤语义与逐行求值全等、计划器不漏报)
//! * FC-QUERY-POST-006(历史视图 TTL 以视图时刻为准)
//! * FC-QUERY-CPLX-001..004(解析 / 计划 / BM25 / 融合复杂度哨兵)
//! * FC-INDEX-INV-005/006/021(等价性 / 过滤先行 / BM25 全局统计)
//! * FC-PERSIST-POST-008(四区落盘与重开一致性)
//! * FC-PERSIST-POST-009(分词口径建库即锁定)
//! * FC-PERSIST-CPLX-004/005(zone map / bloom 评估)
//! * FC-MEM-ERR-002(Fusion 单通道拒绝)
//! * FC-INDEX-PRE-001(bloom_fpp 定义域校验)

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use mneme::{Clock, Expr, Fusion, Mneme, Record, Tuning};
use proptest::prelude::*;

mod common;

use common::{inserted, mem};

/// 可手动推进的假时钟(测试注入,避免依赖真实时间)。
struct FakeClock(AtomicI64);

impl Clock for FakeClock {
    fn now_unix_ms(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// 取命中序列 `(RowId, score)`。
fn ranked(hits: &[mneme::Hit]) -> Vec<(u64, f32)> {
    hits.iter()
        .map(|hit| (hit.rowid.get(), hit.score))
        .collect()
}

/// FC-QUERY-POST-002
#[test]
fn dsl_display_and_json_roundtrip() {
    for text in [
        r#"kind == "preference" && importance > 0.5"#,
        r#"not (a == 1 or b == 2) and c != "x""#,
        r#"k in (1, 2, 3) and tags contains "x""#,
        r#"exists(kind) and not is_null(kind)"#,
        r#"created_at > ts"2024-06-01T00:00:00Z""#,
        r#"s ~ "a*c?" and p startswith "he" and q endswith "lo""#,
        "always",
        "never",
    ] {
        let expr = Expr::from_str(text).unwrap_or_else(|error| panic!("{text}: {error}"));
        let printed = expr.to_string();
        let reparsed = Expr::from_str(&printed).expect("Display 必须可被解析器读回");
        assert_eq!(reparsed, expr, "Display 往返: {text} -> {printed}");
        let decoded = Expr::from_meta(&expr.to_meta()).expect("JSON 必须可解码");
        assert_eq!(decoded, expr, "JSON 往返: {text}");
    }
}

/// FC-QUERY-CPLX-001(长 And/Or 链单遍解析,不递归爆栈、不超时)
#[test]
fn dsl_parse_handles_large_input_once() {
    let mut text = String::new();
    for index in 0..2_000 {
        if index > 0 {
            text.push_str(" or ");
        }
        text.push_str(&format!("f{index} == {index}"));
    }
    let expr = Expr::from_str(&text).expect("2000 项 Or 链");
    assert!(matches!(expr, Expr::Or(_)));
}

/// FC-QUERY-POST-003(IDF / TF 饱和 / 长度归一)
#[test]
fn bm25_formula_behaviour() {
    let ns = mem(2).namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("short").text("alpha"))
        .expect("insert");
    ns.insert(
        Record::new(vec![0.0, 1.0])
            .key("long")
            .text("alpha beta gamma delta epsilon zeta eta theta"),
    )
    .expect("insert");
    ns.insert(Record::new(vec![1.0, 1.0]).key("twice").text("alpha alpha"))
        .expect("insert");
    let hits = ns
        .search()
        .text("alpha")
        .top_k(3)
        .execute()
        .expect("search");
    let by_key: HashMap<String, f32> = hits
        .iter()
        .map(|hit| {
            (
                hit.key
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                hit.score,
            )
        })
        .collect();
    assert!(
        by_key["short"] > by_key["long"],
        "长度归一:同 tf 短文档更优"
    );
    assert!(by_key["twice"] > by_key["short"], "tf 高者更优");
    assert!(
        by_key["twice"] < by_key["short"] * 2.0,
        "tf 饱和:2 倍词频不得带来 2 倍分数"
    );
}

/// FC-QUERY-POST-003(BM25 精确分数:`k1=1.2`、`b=0.75`、IDF 逐位手算)
#[test]
fn bm25_exact_scores_match_formula() {
    let ns = mem(2).namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("d0").text("alpha alpha"))
        .expect("insert");
    ns.insert(
        Record::new(vec![0.0, 1.0])
            .key("d1")
            .text("alpha alpha alpha alpha"),
    )
    .expect("insert");
    let hits = ns
        .search()
        .text("alpha")
        .top_k(2)
        .execute()
        .expect("search");
    // N=2, avgdl=3, df=2 → IDF = ln((2-2+0.5)/(2+0.5)+1) = ln(1.2)。
    let idf = (0.5_f32 / 2.5 + 1.0).ln();
    let norm = |tf: f32, dl: f32| tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * dl / 3.0));
    let expected_d1 = idf * norm(4.0, 4.0);
    let expected_d0 = idf * norm(2.0, 2.0);
    assert_eq!(
        hits[0].key.as_ref().map(ToString::to_string),
        Some("d1".into())
    );
    assert!(
        (hits[0].score - expected_d1).abs() < 1e-5,
        "d1 分数应为 {expected_d1},实得 {}",
        hits[0].score
    );
    assert!(
        (hits[1].score - expected_d0).abs() < 1e-5,
        "d0 分数应为 {expected_d0},实得 {}",
        hits[1].score
    );
}

/// FC-QUERY-POST-004 / FC-MEM-ERR-002(融合可用性与参数校验)
#[test]
fn hybrid_fusion_and_validation() {
    let ns = mem(2).namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a").text("alpha beta"))
        .expect("insert");
    ns.insert(Record::new(vec![0.0, 1.0]).key("b").text("beta"))
        .expect("insert");
    let query = [1.0_f32, 0.0];

    // Fusion 需要双通道:单通道设置即拒绝,绝不静默忽略。
    assert!(matches!(
        ns.search()
            .vector(&query)
            .fusion(Fusion::default())
            .execute(),
        Err(mneme::MnemeError::Config { .. })
    ));
    assert!(matches!(
        ns.search()
            .text("alpha")
            .fusion(Fusion::default())
            .execute(),
        Err(mneme::MnemeError::Config { .. })
    ));

    // 双通道默认融合 ≡ 显式 Rrf{k:60},排序全等。
    let default_hits = ns
        .search()
        .vector(&query)
        .text("alpha beta")
        .top_k(2)
        .execute()
        .expect("dual channel");
    let explicit = ns
        .search()
        .vector(&query)
        .text("alpha beta")
        .fusion(Fusion::Rrf { k: 60 })
        .top_k(2)
        .execute()
        .expect("explicit rrf");
    assert_eq!(ranked(&default_hits), ranked(&explicit));

    // Weighted alpha 非 [0,1] 内有限值 → Config(含 NaN/Inf)。
    for alpha in [1.5_f32, -0.5, f32::NAN, f32::INFINITY] {
        assert!(
            matches!(
                ns.search()
                    .vector(&query)
                    .text("beta")
                    .fusion(Fusion::Weighted { alpha })
                    .execute(),
                Err(mneme::MnemeError::Config { .. })
            ),
            "alpha = {alpha} 必须拒绝"
        );
    }
    assert!(
        ns.search()
            .vector(&query)
            .text("beta")
            .fusion(Fusion::Weighted { alpha: 0.5 })
            .top_k(2)
            .execute()
            .is_ok()
    );
}

/// FC-INDEX-INV-006(过滤先行:候选集内融合,绝不"先融合截断再过滤")
#[test]
fn filter_is_order_independent() {
    let ns = mem(2).namespace("n");
    // 被过滤的低重要度记录在向量与文本通道都更优,全量融合 top-3 全是它们:
    // 若实现"先融合截断再过滤",top_k=3 会一条不剩;过滤先行必须返回 3 条。
    for index in 0..6 {
        let important = index % 2 == 0;
        let text = if important {
            "alpha beta gamma delta epsilon"
        } else {
            "alpha"
        };
        // 两个通道都偏好低重要度记录(构造"后过滤"必失败的场景):
        // 向量方向不同(cosine 区分),文本为短文档(BM25 更高)。
        let vector = if important {
            vec![0.1, 1.0]
        } else {
            vec![1.0, 0.0]
        };
        ns.insert(
            Record::new(vector)
                .key(format!("k{index}"))
                .text(text)
                .importance(if important { 0.9 } else { 0.1 }),
        )
        .expect("insert");
    }
    let query = [1.0_f32, 0.0];
    let filter = Expr::field("importance").gt(0.5_f32);
    let filtered = ns
        .search()
        .vector(&query)
        .text("alpha")
        .filter(filter)
        .top_k(3)
        .execute()
        .expect("filtered");
    assert_eq!(filtered.len(), 3, "过滤先行后候选仍有 3 条");
    assert!(
        filtered.iter().all(|hit| hit.importance > 0.5),
        "结果必须全部满足过滤"
    );
    // 证伪对照:全量融合 top-3 全是被过滤记录,后过滤实现会返回 0 条。
    let all = ns
        .search()
        .vector(&query)
        .text("alpha")
        .top_k(3)
        .execute()
        .expect("unfiltered");
    assert!(
        all.iter().all(|hit| hit.importance <= 0.5),
        "全量 top-3 应全部低重要度,构造才有证伪力"
    );
}

/// FC-INDEX-INV-021(NS 隔离:其他命名空间的文档量不得影响统计)
#[test]
fn bm25_statistics_are_namespace_isolated() {
    let db = Mneme::in_memory(2).expect("in_memory");
    let a = db.namespace("a");
    let b = db.namespace("b");
    for index in 0..5 {
        a.insert(
            Record::new(vec![1.0, 0.0])
                .key(format!("a{index}"))
                .text("shared alpha"),
        )
        .expect("insert");
    }
    let before = ranked(
        &a.search()
            .text("shared")
            .top_k(10)
            .execute()
            .expect("search"),
    );
    for index in 0..50 {
        b.insert(
            Record::new(vec![0.0, 1.0])
                .key(format!("b{index}"))
                .text("shared shared filler filler filler"),
        )
        .expect("insert");
    }
    let after = ranked(
        &a.search()
            .text("shared")
            .top_k(10)
            .execute()
            .expect("search"),
    );
    assert_eq!(before, after, "其他 NS 的 df/N/avgdl 不得进入查询统计");
}

/// FC-PERSIST-POST-008(四区落盘:flush→关闭→重开,BM25 与过滤结果逐位一致)
#[test]
fn text_index_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Mneme::builder()
        .path(dir.path())
        .dimension(2)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    for index in 0..12 {
        ns.insert(
            Record::new(vec![index as f32, 1.0])
                .key(format!("k{index}"))
                .text(format!("alpha memory document {index}")),
        )
        .expect("insert");
    }
    for index in 0..4 {
        ns.insert(
            Record::new(vec![0.0, 1.0])
                .key(format!("b{index}"))
                .text("beta only"),
        )
        .expect("insert");
    }
    let before = ranked(
        &ns.search()
            .text("alpha")
            .top_k(20)
            .execute()
            .expect("search"),
    );
    db.close().expect("close");

    let db = Mneme::open(dir.path()).expect("open");
    let ns = db.namespace("n");
    let after = ranked(
        &ns.search()
            .text("alpha")
            .top_k(20)
            .execute()
            .expect("search"),
    );
    assert_eq!(before, after, "重开后 BM25 结果与分数逐位一致");

    // 过滤 + 计划器(bloom/zone map)在重开后仍正确。
    let filtered = ns
        .search()
        .text("alpha")
        .filter(Expr::field("key").eq("k3"))
        .execute()
        .expect("filtered");
    assert_eq!(filtered.len(), 1);
    assert_eq!(
        filtered[0].key.as_ref().map(ToString::to_string),
        Some("k3".to_string())
    );

    // 重开后新写入的文本走增量倒排,无需再 flush 即可检索。
    ns.insert(
        Record::new(vec![1.0, 0.0])
            .key("newcomer")
            .text("alpha newcomer"),
    )
    .expect("insert");
    assert_eq!(
        ns.search()
            .text("alpha")
            .top_k(30)
            .execute()
            .expect("search")
            .len(),
        13
    );
}

/// FC-PERSIST-POST-008 / FC-INDEX-INV-021(段 + 增量共用同一全局 BM25 统计)
#[test]
fn flushed_and_tail_records_share_bm25_statistics() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Mneme::builder()
        .path(dir.path())
        .dimension(2)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(Record::new(vec![1.0, 0.0]).key("old").text("alpha alpha"))
        .expect("insert");
    db.flush().expect("flush");
    ns.insert(
        Record::new(vec![0.0, 1.0])
            .key("new")
            .text("alpha alpha alpha alpha"),
    )
    .expect("insert");
    let before = ranked(
        &ns.search()
            .text("alpha")
            .top_k(2)
            .execute()
            .expect("search"),
    );
    assert_eq!(before.len(), 2);
    // 再 flush 使全部记录落段:同一查询的分数必须逐位一致。若统计只算尾部
    // 增量(N/avgdl 口径不同),分数会变,此处即失败。
    db.flush().expect("flush");
    let after = ranked(
        &ns.search()
            .text("alpha")
            .top_k(2)
            .execute()
            .expect("search"),
    );
    assert_eq!(before, after, "flush 前后同一查询分数逐位一致");
    assert_eq!(after[0].0, 1, "tf 高且更短的增量记录居首");
}

/// FC-QUERY-POST-005(过滤语义:count 与计划器检索的行集合一致)
#[test]
fn plan_filter_matches_pointwise_count() {
    let ns = mem(2).namespace("n");
    for index in 0..40 {
        ns.insert(
            Record::new(vec![1.0, 0.0])
                .key(format!("k{index}"))
                .importance(index as f32 / 39.0)
                .metadata(mneme::json!({"rank": index})),
        )
        .expect("insert");
    }
    for filter in [
        Expr::field("importance").gt(0.5_f32),
        Expr::field("rank").le(10_i64),
        Expr::field("importance").gt(0.2_f32) & Expr::field("rank").lt(30_i64),
        Expr::Not(Box::new(Expr::field("importance").gt(0.5_f32))),
        Expr::Exists("rank".into()),
    ] {
        let count = ns.count(Some(filter.clone())).expect("count");
        let hits = ns
            .search()
            .vector(&[1.0, 0.0])
            .filter(filter)
            .top_k(4096)
            .execute()
            .expect("search");
        assert_eq!(count as usize, hits.len(), "计划器不得改变过滤语义");
    }
}

/// FC-QUERY-POST-005(块级下推绝不漏报:大整数、类型污染、非数值 `exists`)
#[test]
fn planner_never_prunes_possible_blocks() {
    let ns = mem(2).namespace("n");
    // 前 2047 行数字 rank 占满块 0/1;大整数在块 1;字符串 rank 独占块 2。
    // 旧实现会因 f64 精度/非数值不观察而剪掉这些块,导致漏报。
    for index in 0..2047 {
        ns.insert(
            Record::new(vec![1.0, 0.0])
                .key(format!("k{index}"))
                .metadata(mneme::json!({"rank": index})),
        )
        .expect("insert");
    }
    ns.insert(
        Record::new(vec![1.0, 0.0])
            .key("big")
            .metadata(mneme::json!({"rank": 9_007_199_254_740_993_i64})),
    )
    .expect("insert");
    ns.insert(
        Record::new(vec![1.0, 0.0])
            .key("str")
            .metadata(mneme::json!({"rank": "high"})),
    )
    .expect("insert");
    // metadata 数字污染保留字段 `key` 的 zone 类别;字符串 != 比较不得被剪空。
    ns.insert(
        Record::new(vec![1.0, 0.0])
            .key("polluted")
            .metadata(mneme::json!({"key": 5})),
    )
    .expect("insert");

    let cases = [
        // 行级 i64 精确比较命中大整数(块级曾因 f64 舍入误剪)。
        (Expr::field("rank").gt(9_007_199_254_740_992_i64), 1_usize),
        // 字符串 rank 所在块必须保留(曾因只统计数值而误剪)。
        (Expr::Exists("rank".into()), 2049),
        // `key` 被 metadata 数字污染后,字符串比较仍走全量位图。
        (Expr::field("key").ne("nope"), 2050),
    ];
    for (filter, expected) in cases {
        let count = ns.count(Some(filter.clone())).expect("count");
        let hits = ns
            .search()
            .vector(&[1.0, 0.0])
            .filter(filter)
            .top_k(4096)
            .execute()
            .expect("search");
        assert_eq!(count as usize, expected, "逐行求值口径");
        assert_eq!(hits.len(), expected, "计划器不得漏报");
    }
}

/// FC-QUERY-POST-006(历史视图的 TTL 判定以视图时刻为准,与墙上时钟无关)
#[test]
fn historical_search_uses_view_time_for_ttl() {
    let clock = Arc::new(FakeClock(AtomicI64::new(1_000_000)));
    let db = Mneme::builder()
        .dimension(2)
        .clock(Arc::clone(&clock) as Arc<dyn Clock>)
        .build()
        .expect("build");
    let ns = db.namespace("n");
    ns.insert(
        Record::new(vec![1.0, 0.0])
            .key("ephemeral")
            .ttl(Duration::from_millis(100)),
    )
    .expect("insert");

    // 墙上时钟推进到 TTL 之后:普通查询不可见。
    clock.0.store(1_000_200, Ordering::Relaxed);
    assert!(
        ns.search()
            .vector(&[1.0, 0.0])
            .execute()
            .expect("now search")
            .is_empty(),
        "过期记录在当下不可见"
    );

    // as_of(1_000_050) < expires_at(1_000_100):历史视图必须可见。
    let hits = ns
        .search()
        .vector(&[1.0, 0.0])
        .as_of(1_000_050)
        .execute()
        .expect("as_of search");
    assert_eq!(hits.len(), 1, "历史视图按视图时刻判 TTL");
    assert_eq!(
        hits[0].key.as_ref().map(ToString::to_string),
        Some("ephemeral".to_string())
    );

    // SnapshotHandle 路径同样以快照时刻判 TTL。
    let snap = db.as_of(1_000_050).expect("snapshot");
    let ns = snap.namespace("n");
    let hits = ns
        .search()
        .vector(&[1.0, 0.0])
        .execute()
        .expect("snapshot search");
    assert_eq!(hits.len(), 1, "快照检索按快照时刻判 TTL");
    assert_eq!(ns.count(None).expect("count"), 1, "点读/统计同快照时刻口径");
    assert_eq!(
        ns.iter(None).expect("iter").count(),
        1,
        "快照遍历同快照时刻口径"
    );
    assert!(
        ns.get("ephemeral").expect("get").is_some(),
        "快照点读不得早于快照时刻过期"
    );
    // 对照:同一记录在当下已过期。
    let now = db.snapshot().namespace("n");
    assert_eq!(now.count(None).expect("count"), 0);
    assert_eq!(now.iter(None).expect("iter").count(), 0);
    assert!(now.get("ephemeral").expect("get").is_none());
}

/// FC-PERSIST-POST-008(超 `f64` 精确范围的整数落段后 fail-fast 重开:±∞ 区间合法)
#[test]
fn lossy_zone_intervals_survive_fail_fast_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Mneme::builder()
        .path(dir.path())
        .dimension(2)
        .build()
        .expect("build");
    db.namespace("n")
        .insert(
            Record::new(vec![1.0, 0.0])
                .key("big")
                .metadata(mneme::json!({"rank": 9_007_199_254_740_993_i64})),
        )
        .expect("insert");
    db.flush().expect("flush");
    db.close().expect("close");

    // fail-fast 下自产段必须可重开(zone 区间放宽为 ±∞ 是合法编码,不是损坏)。
    let db = Mneme::builder()
        .path(dir.path())
        .dimension(2)
        .fail_fast_on_corruption(true)
        .build()
        .expect("fail-fast reopen");
    let hits = db
        .namespace("n")
        .search()
        .vector(&[1.0, 0.0])
        .filter(Expr::field("rank").gt(9_007_199_254_740_992_i64))
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 1, "大整数过滤在重开后仍命中");
}

/// FC-PERSIST-POST-009(分词口径建库即锁定:重开忽略调用方冲突配置)
#[test]
fn stopwords_setting_is_locked_at_creation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tuning = Tuning {
        stopwords: false,
        ..Tuning::default()
    };
    let db = Mneme::builder()
        .path(dir.path())
        .dimension(2)
        .tuning(tuning)
        .build()
        .expect("build");
    db.namespace("n")
        .insert(Record::new(vec![1.0, 0.0]).key("a").text("alpha the beta"))
        .expect("insert");
    db.close().expect("close");

    // 重开时故意传 `stopwords: true`;以 MANIFEST 记录的建库配置为准,
    // 索引与查询同口径 → "the" 仍可检索。
    let reopened = Mneme::builder()
        .path(dir.path())
        .dimension(2)
        .tuning(Tuning {
            stopwords: true,
            ..Tuning::default()
        })
        .build()
        .expect("reopen");
    let hits = reopened
        .namespace("n")
        .search()
        .text("the")
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 1, "建库停用词配置必须锁定,查询分词同口径");
}

/// FC-INDEX-PRE-001(bloom_fpp 定义域:0/1/非有限值 → Config)
#[test]
fn builder_rejects_invalid_bloom_fpp() {
    for fpp in [0.0_f32, 1.0, -0.5, f32::NAN, f32::INFINITY] {
        let tuning = Tuning {
            bloom_fpp: fpp,
            ..Tuning::default()
        };
        assert!(
            matches!(
                Mneme::builder().dimension(2).tuning(tuning).build(),
                Err(mneme::MnemeError::Config { .. })
            ),
            "bloom_fpp = {fpp} 必须拒绝"
        );
    }
    // field_dict_max = 0 无法容纳 `key` 字段(自产 bloom 缺失),必须拒绝。
    let tuning = Tuning {
        field_dict_max: 0,
        ..Tuning::default()
    };
    assert!(
        matches!(
            Mneme::builder().dimension(2).tuning(tuning).build(),
            Err(mneme::MnemeError::Config { .. })
        ),
        "field_dict_max = 0 必须拒绝"
    );
}

proptest! {
    /// FC-QUERY-ERR-001(任意输入不 panic,错误必为结构化 FilterParse)
    #[test]
    fn dsl_never_panics_on_arbitrary_input(input in ".{0,200}") {
        match Expr::from_str(&input) {
            Ok(expr) => {
                let printed = expr.to_string();
                let _ = Expr::from_meta(&expr.to_meta());
                prop_assert!(Expr::from_str(&printed).is_ok(), "打印结果必须可重解析");
            }
            Err(mneme::MnemeError::FilterParse(_)) => {}
            Err(other) => prop_assert!(false, "非 FilterParse 错误: {other:?}"),
        }
    }

    /// FC-INDEX-INV-005/006(候选集内 ANN ≡ 过滤后参考暴力)
    #[test]
    fn vector_channel_matches_bruteforce(
        records in prop::collection::vec(
            (
                prop::collection::vec(-1.0f32..1.0, 4),
                any::<bool>(),
            ),
            1..24,
        ),
        query in prop::collection::vec(-1.0f32..1.0, 4),
        k in 1usize..8,
    ) {
        let db = Mneme::builder()
            .dimension(4)
            .metric(mneme::Metric::Dot)
            .build()
            .expect("build");
        let ns = db.namespace("n");
        let mut expected = Vec::new();
        for (vector, keep) in &records {
            let id = inserted(
                ns.insert(
                    Record::new(vector.clone())
                        .importance(if *keep { 0.9 } else { 0.1 }),
                )
                .expect("insert"),
            );
            if *keep {
                expected.push((id.get(), vector.clone()));
            }
        }
        let hits = ns
            .search()
            .vector(&query)
            .filter(Expr::field("importance").gt(0.5_f32))
            .top_k(k)
            .execute()
            .expect("search");
        let got: Vec<u64> = hits.iter().map(|hit| hit.rowid.get()).collect();
        prop_assert_eq!(got, common::reference_dot(&query, &expected, k));
    }
}
