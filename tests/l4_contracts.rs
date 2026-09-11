//! L4 检索层契约验收测试。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-QUERY-ERR-001(DSL 任意输入不 panic)
//! * FC-QUERY-POST-002(Display / JSON 往返)
//! * FC-QUERY-POST-003(BM25 公式行为)
//! * FC-QUERY-POST-004(融合与参数校验)
//! * FC-QUERY-POST-005(过滤语义与逐行求值全等)
//! * FC-QUERY-CPLX-001..004(解析 / 计划 / BM25 / 融合复杂度哨兵)
//! * FC-INDEX-INV-005/006/021(等价性 / 过滤先行 / BM25 全局统计)
//! * FC-PERSIST-POST-008(四区落盘与重开一致性)
//! * FC-PERSIST-CPLX-004/005(zone map / bloom 评估)
//! * FC-MEM-ERR-002(Fusion 单通道拒绝)

use std::collections::HashMap;

use mneme::{Expr, Fusion, Mneme, Record};
use proptest::prelude::*;

mod common;

use common::{inserted, mem};

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

/// FC-INDEX-INV-006(过滤先行:与"先融合后过滤"结果一致,大 top_k 避免截断)
#[test]
fn filter_is_order_independent() {
    let ns = mem(2).namespace("n");
    for index in 0..6 {
        ns.insert(
            Record::new(vec![1.0, 0.0])
                .key(format!("k{index}"))
                .text("alpha")
                .importance(if index % 2 == 0 { 0.9 } else { 0.1 }),
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
        .top_k(64)
        .execute()
        .expect("filtered");
    let all = ns
        .search()
        .vector(&query)
        .text("alpha")
        .top_k(64)
        .execute()
        .expect("unfiltered");
    let got: Vec<u64> = filtered.iter().map(|hit| hit.rowid.get()).collect();
    let manual: Vec<u64> = all
        .iter()
        .filter(|hit| hit.importance > 0.5)
        .map(|hit| hit.rowid.get())
        .collect();
    assert_eq!(got, manual, "过滤先行与融合后过滤结果全等");
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

/// FC-PERSIST-POST-008(持久库中段 + 增量统一参与 BM25 统计)
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
    let hits = ns
        .search()
        .text("alpha")
        .top_k(2)
        .execute()
        .expect("search");
    assert_eq!(hits.len(), 2);
    // 增量记录 tf 更高且更短 → 居首;段与增量共用同一全局统计。
    assert_eq!(
        hits[0].key.as_ref().map(ToString::to_string),
        Some("new".to_string())
    );
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

    /// FC-INDEX-INV-005(单通道向量检索 ≡ 参考暴力实现)
    #[test]
    fn vector_channel_matches_bruteforce(
        records in prop::collection::vec(prop::collection::vec(-1.0f32..1.0, 4), 1..24),
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
        for vector in &records {
            let id = inserted(ns.insert(Record::new(vector.clone())).expect("insert"));
            expected.push((id.get(), vector.clone()));
        }
        let hits = ns.search().vector(&query).top_k(k).execute().expect("search");
        let got: Vec<u64> = hits.iter().map(|hit| hit.rowid.get()).collect();
        prop_assert_eq!(got, common::reference_dot(&query, &expected, k));
    }
}
