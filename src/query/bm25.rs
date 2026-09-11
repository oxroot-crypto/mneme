//! BM25 关键词通道(设计 06 §3)。
//!
//! 两遍法:第一遍按查询命名空间**全局**统计 `N`/`avgdl`(只计可见行)与
//! 查询词的 `df`;第二遍用同一套统计对 postings 打分取 top-k。命名空间
//! 隔离与"只计活行"由 [`InvertedIndex`](crate::memory::analysis::InvertedIndex)
//! 的分桶结构与视图可见性过滤共同保证(I21)。

use std::collections::HashMap;

use crate::core::bitset::BitSet;
use crate::core::heap::TopK;
use crate::core::metric::Metric;
use crate::core::text::tokenize;
use crate::core::types::{NsId, SlotId};
use crate::memory::search::Scored;
use crate::memory::table::ReaderView;

/// BM25 词频饱和参数 `k1`(设计 06 §3.2 默认值)。
const K1: f32 = 1.2;
/// BM25 文档长度归一参数 `b`(设计 06 §3.2 默认值)。
const B: f32 = 0.75;

/// 一次 BM25 查询的全部输入。
pub(crate) struct Bm25Query<'a> {
    /// 不可变读视图。
    pub(crate) view: &'a ReaderView,
    /// 目标命名空间。
    pub(crate) ns_id: NsId,
    /// 查询文本。
    pub(crate) query: &'a str,
    /// 本通道返回条数。
    pub(crate) top_k: usize,
    /// 当前时刻(Unix 毫秒;TTL 可见性)。
    pub(crate) now_ms: i64,
    /// 是否启用停用词过滤(与索引构建同口径)。
    pub(crate) stopwords: bool,
    /// 过滤先行候选位图(全局槽位);`None` = 无过滤。
    pub(crate) candidates: Option<&'a BitSet>,
}

/// 执行 BM25 检索,返回按分数降序(同分 `RowId` 升序)的命中。
///
/// 查询文本分词后无有效词、命名空间无文本记录或候选为空时返回空列表。
pub(crate) fn search(params: &Bm25Query<'_>) -> Vec<Scored> {
    if params.top_k == 0 {
        return Vec::new();
    }
    let Some(docs) = params.view.inv.doc_entries(params.ns_id) else {
        return Vec::new();
    };
    let query_terms = query_terms(params.query, params.stopwords);
    if query_terms.is_empty() {
        return Vec::new();
    }
    let Some(stats) = collect_stats(params, docs, &query_terms) else {
        return Vec::new();
    };
    let scores = score_postings(params, &query_terms, docs, &stats);
    rank_scores(params, &scores)
}

/// 查询词分词后去重(排序保证确定性)。
fn query_terms(query: &str, stopwords: bool) -> Vec<String> {
    let mut terms = tokenize(query, stopwords);
    terms.sort();
    terms.dedup();
    terms
}

/// 第一遍的全局统计。
struct Bm25Stats {
    /// 查询命名空间内可见的文本记录数。
    n: u64,
    /// 平均文档词数。
    avgdl: f32,
    /// 各查询词的全局文档频率。
    df: HashMap<String, u64>,
}

/// 第一遍:统计 `N`/`avgdl`(只计可见行)与查询词 `df`。
fn collect_stats(
    params: &Bm25Query<'_>,
    docs: &HashMap<SlotId, u32>,
    query_terms: &[String],
) -> Option<Bm25Stats> {
    let mut n = 0_u64;
    let mut total_dl = 0_u64;
    for slot in docs.keys() {
        if is_visible(params, *slot) {
            n += 1;
            total_dl += u64::from(docs[slot]);
        }
    }
    if n == 0 {
        return None;
    }
    let avgdl = total_dl as f32 / n as f32;
    let mut df = HashMap::new();
    for term in query_terms {
        let count = params
            .view
            .inv
            .postings_of(params.ns_id, term)
            .map_or(0, |postings| {
                postings
                    .iter()
                    .filter(|posting| is_visible(params, posting.slot))
                    .count() as u64
            });
        df.insert(term.clone(), count);
    }
    Some(Bm25Stats { n, avgdl, df })
}

/// 第二遍:用全局统计对候选内的可见 postings 打分。
fn score_postings(
    params: &Bm25Query<'_>,
    terms: &[String],
    docs: &HashMap<SlotId, u32>,
    stats: &Bm25Stats,
) -> HashMap<SlotId, f32> {
    let mut scores: HashMap<SlotId, f32> = HashMap::new();
    for term in terms {
        let Some(postings) = params.view.inv.postings_of(params.ns_id, term) else {
            continue;
        };
        let Some(&df) = stats.df.get(term) else {
            continue;
        };
        let idf = idf(stats.n, df);
        for posting in postings {
            if !is_visible(params, posting.slot) || !is_candidate(params, posting.slot) {
                continue;
            }
            let doc_len = docs.get(&posting.slot).copied().unwrap_or(0) as f32;
            let tf = posting.tf as f32;
            let norm = tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * doc_len / stats.avgdl));
            *scores.entry(posting.slot).or_insert(0.0) += idf * norm;
        }
    }
    scores
}

/// IDF:`ln((N - df + 0.5)/(df + 0.5) + 1)`。
fn idf(n: u64, df: u64) -> f32 {
    (((n as f64 - df as f64 + 0.5) / (df as f64 + 0.5)) + 1.0).ln() as f32
}

/// 按分数取 top-k(同分 `RowId` 升序)。
fn rank_scores(params: &Bm25Query<'_>, scores: &HashMap<SlotId, f32>) -> Vec<Scored> {
    let mut top = TopK::new(params.top_k, Metric::Dot);
    for (slot, score) in scores {
        let rowid = params.view.slots[slot.get() as usize].rowid;
        top.push(*score, (rowid, *slot));
    }
    top.into_sorted_vec()
        .into_iter()
        .map(|(rowid, slot)| Scored {
            slot,
            rowid,
            // `slot` 是上一步由 `scores` 自身的键 push 进 TopK 的,查询必然命中;
            // `unwrap_or` 只为无 panic 的全函数形态,不构成静默兜底路径。
            score: scores.get(&slot).copied().unwrap_or(0.0),
        })
        .collect()
}

/// 槽位在当前视图与命名空间下是否可见(未墓碑、未过期、非遮蔽版本)。
fn is_visible(params: &Bm25Query<'_>, slot: SlotId) -> bool {
    let index = slot.get() as usize;
    let Some(slot_data) = params.view.slots.get(index) else {
        return false;
    };
    !params.view.dead.get(index)
        && slot_data.ns_id == params.ns_id
        && slot_data.is_live(params.now_ms)
}

/// 槽位是否在过滤先行候选集合内。
fn is_candidate(params: &Bm25Query<'_>, slot: SlotId) -> bool {
    params
        .candidates
        .is_none_or(|bits| bits.get(slot.get() as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::RowId;
    use crate::memory::Mneme;
    use crate::memory::Record;

    /// 从当前视图解析命名空间 `n` 的 `NsId`。
    fn ns_id_of(db: &Mneme) -> NsId {
        let view = db.table.view();
        view.ns_registry
            .iter()
            .find_map(|(id, path)| (**path == *"n").then_some(*id))
            .expect("命名空间已注册")
    }

    /// 在当前视图上执行 BM25 查询。
    fn search_now(db: &Mneme, ns_id: NsId, text: &str, top_k: usize) -> Vec<Scored> {
        let view = db.table.view();
        search(&Bm25Query {
            view: &view,
            ns_id,
            query: text,
            top_k,
            now_ms: 0,
            stopwords: false,
            candidates: None,
        })
    }

    /// FC-QUERY-POST-003(IDF:稀有词得分更高)
    #[test]
    fn rare_term_ranks_above_common_term() {
        let db = Mneme::in_memory(2).expect("in_memory");
        let ns = db.namespace("n");
        ns.insert(Record::new(vec![1.0, 0.0]).text("alpha beta"))
            .expect("insert");
        ns.insert(Record::new(vec![0.0, 1.0]).text("beta"))
            .expect("insert");
        ns.insert(Record::new(vec![1.0, 1.0]).text("beta"))
            .expect("insert");
        let hits = search_now(&db, ns_id_of(&db), "alpha beta", 3);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].rowid, RowId::new(0), "含稀有词者居首");
    }

    /// FC-QUERY-POST-003(TF 饱和:重复不线性加分)
    #[test]
    fn term_frequency_saturates() {
        let db = Mneme::in_memory(2).expect("in_memory");
        let ns = db.namespace("n");
        ns.insert(Record::new(vec![1.0, 0.0]).text("alpha"))
            .expect("insert");
        ns.insert(Record::new(vec![0.0, 1.0]).text("alpha alpha alpha"))
            .expect("insert");
        let hits = search_now(&db, ns_id_of(&db), "alpha", 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].rowid, RowId::new(1), "tf 高者更优");
        // 饱和:3 倍 tf 的分数增幅 < 3 倍。
        assert!(hits[0].score < hits[1].score * 3.0);
    }

    /// FC-QUERY-POST-003(长度归一:同 tf 短文档得分更高)
    #[test]
    fn shorter_document_wins_at_equal_tf() {
        let db = Mneme::in_memory(2).expect("in_memory");
        let ns = db.namespace("n");
        ns.insert(Record::new(vec![1.0, 0.0]).text("alpha"))
            .expect("insert");
        ns.insert(
            Record::new(vec![0.0, 1.0])
                .text("alpha x1 x2 x3 x4 x5 x6 x7 x8 x9 x10 x11 x12 x13 x14 x15"),
        )
        .expect("insert");
        let hits = search_now(&db, ns_id_of(&db), "alpha", 2);
        assert_eq!(hits[0].rowid, RowId::new(0), "短文档居首");
    }

    /// FC-INDEX-INV-021(只计可见行:墓碑不计入 df/N)
    #[test]
    fn deleted_records_are_not_counted() {
        let db = Mneme::in_memory(2).expect("in_memory");
        let ns = db.namespace("n");
        ns.insert(Record::new(vec![1.0, 0.0]).key("a").text("alpha"))
            .expect("insert");
        ns.insert(Record::new(vec![0.0, 1.0]).key("b").text("alpha"))
            .expect("insert");
        let ns_id = ns_id_of(&db);
        let before = search_now(&db, ns_id, "alpha", 2);
        ns.delete("a").expect("delete");
        let after = search_now(&db, ns_id, "alpha", 2);
        assert_eq!(before.len(), 2);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].rowid, RowId::new(1), "墓碑不再参与统计与打分");
    }
}
