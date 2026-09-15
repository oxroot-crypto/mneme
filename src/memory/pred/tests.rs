use super::*;
use crate::memory::engine::Mneme;
use crate::memory::record::Record;

#[test]
fn is_in_deduplicates_and_matches_listed_values() {
    let db = Mneme::in_memory(2).expect("in_memory");
    let ns = db.namespace("t");
    for key in ["a", "b", "c"] {
        ns.insert(Record::new(vec![1.0, 0.0]).key(key))
            .expect("insert");
    }
    let expr = Expr::field("key").is_in(["a", "a", "c"].map(Val::from));
    match &expr {
        Expr::In(_, vals) => assert_eq!(vals.len(), 2, "重复候选值按首次出现去重"),
        other => panic!("期望 In,得到 {other:?}"),
    }
    assert_eq!(ns.count(Some(expr)).expect("count"), 2);
}

/// FC-QUERY-POST-009:合取链按预估选择性升序稳定重排;`Or`/`Not` 结构保持。
#[test]
fn reorder_sorts_conjuncts_by_estimated_selectivity() {
    let key_eq = Expr::field("key").eq("k");
    let contains = Expr::Contains("text".into(), Val::Str("x".into()));
    let exists = Expr::Exists("kind".into());
    let reordered = reorder_for_eval(&Expr::And(
        vec![exists.clone(), contains.clone(), key_eq.clone()].into_boxed_slice(),
    ));
    assert_eq!(
        reordered,
        Expr::And(vec![key_eq.clone(), contains.clone(), exists.clone()].into_boxed_slice()),
        "高选择等值最前、低选择 Exists 最后"
    );

    // 同估值保持输入相对顺序(稳定排序);嵌套 `And` 递归重排。
    let nested = Expr::And(
        vec![
            Expr::Or(vec![contains.clone(), key_eq.clone()].into_boxed_slice()),
            Expr::And(vec![exists.clone(), key_eq.clone()].into_boxed_slice()),
        ]
        .into_boxed_slice(),
    );
    let reordered = reorder_for_eval(&nested);
    let Expr::And(parts) = &reordered else {
        panic!("重排后仍为 And");
    };
    assert_eq!(
        parts[0],
        Expr::And(vec![key_eq.clone(), exists.clone()].into_boxed_slice()),
        "内层 And 重排后整体估值更低,外层应前移"
    );
    assert_eq!(
        parts[1],
        Expr::Or(vec![contains, key_eq].into_boxed_slice()),
        "Or 结构保持原顺序"
    );
}
