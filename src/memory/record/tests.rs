use super::*;
use crate::memory::engine::Mneme;
use crate::memory::score::ScoreBreakdown;

#[test]
fn explain_returns_breakdown_or_default() {
    let db = Mneme::in_memory(2).expect("in_memory");
    let ns = db.namespace("t");
    ns.insert(Record::new(vec![1.0, 0.0]).key("a"))
        .expect("insert");
    let hits = ns
        .search()
        .vector(&[1.0, 0.0])
        .top_k(1)
        .execute()
        .expect("search");
    assert_eq!(
        hits[0].explain(),
        ScoreBreakdown {
            sim: hits[0].score,
            ..ScoreBreakdown::default()
        },
        "未开启 Scoring 时明细只有相似度分,其余因子为 0"
    );
}
