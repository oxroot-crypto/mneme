//! 内存倒排索引(`analysis/inv.rs`,设计 04 §5.4、06 §3.4)。
//!
//! 结构:`NsId → term → [(SlotId, tf)]` 与 `SlotId → 文档词数`。写路径在
//! [`WriterState::commit_version`](super::super::table::WriterState) 时增量插入;
//! 旧版本槽位保留在 postings 中,由查询期按视图可见性过滤(支持 `as_of`)。
//!
//! 倒排按命名空间分桶,BM25 统计天然满足 NS 隔离(I21)。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::text::tokenize;
use crate::core::types::{NsId, SlotId};

/// 一条 posting:物理槽位与词频。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Posting {
    /// 物理槽位。
    pub(crate) slot: SlotId,
    /// 词在该文档中的出现次数。
    pub(crate) tf: u32,
}

/// 内存倒排索引。
#[derive(Debug, Default, Clone)]
pub(crate) struct InvertedIndex {
    /// 命名空间 → 词 → postings(SlotId 近似升序,更新追加在后)。
    terms: HashMap<NsId, HashMap<Arc<str>, Vec<Posting>>>,
    /// 命名空间 → (物理槽位 → 文档词数);仅含非空文本的记录。
    docs: HashMap<NsId, HashMap<SlotId, u32>>,
}

impl InvertedIndex {
    /// 把一条记录加入索引:分词并累计词频;空文本不登记。
    ///
    /// # Arguments
    /// * `slot` - 物理槽位。
    /// * `ns_id` - 记录所属命名空间。
    /// * `text` - 原始文本。
    /// * `stopwords` - 是否启用内置停用词过滤。
    pub(crate) fn insert_text(&mut self, slot: SlotId, ns_id: NsId, text: &str, stopwords: bool) {
        let tokens = tokenize(text, stopwords);
        if tokens.is_empty() {
            return;
        }
        self.docs
            .entry(ns_id)
            .or_default()
            .insert(slot, tokens.len() as u32);
        let bucket = self.terms.entry(ns_id).or_default();
        for token in tokens {
            let postings = bucket.entry(Arc::from(token.as_str())).or_default();
            match postings.last_mut() {
                Some(last) if last.slot == slot => last.tf += 1,
                _ => postings.push(Posting { slot, tf: 1 }),
            }
        }
    }

    /// 返回某命名空间"物理槽位 → 文档词数"表(未按可见性过滤)。
    pub(crate) fn doc_entries(&self, ns_id: NsId) -> Option<&HashMap<SlotId, u32>> {
        self.docs.get(&ns_id)
    }

    /// 返回某命名空间中指定词的 postings。
    pub(crate) fn postings_of(&self, ns_id: NsId, term: &str) -> Option<&[Posting]> {
        self.terms
            .get(&ns_id)
            .and_then(|bucket| bucket.get(term))
            .map(Vec::as_slice)
    }

    /// 已登记的命名空间(落盘编码用)。
    pub(crate) fn ns_ids(&self) -> impl Iterator<Item = NsId> + '_ {
        self.docs.keys().copied()
    }

    /// 返回某命名空间的全部词表(落盘编码用)。
    pub(crate) fn terms_of(&self, ns_id: NsId) -> Option<&HashMap<Arc<str>, Vec<Posting>>> {
        self.terms.get(&ns_id)
    }

    /// 直接插入一条已聚合的 posting 列表(倒排落盘解码用)。
    ///
    /// 调用方保证 postings 按 `SlotId` 升序且同一槽位已聚合 tf。
    pub(crate) fn insert_term(&mut self, ns_id: NsId, term: Arc<str>, postings: Vec<Posting>) {
        self.terms.entry(ns_id).or_default().insert(term, postings);
    }

    /// 登记一个文档词数(倒排落盘解码用)。
    pub(crate) fn insert_doc(&mut self, ns_id: NsId, slot: SlotId, doc_len: u32) {
        self.docs.entry(ns_id).or_default().insert(slot, doc_len);
    }

    /// 合并另一倒排索引(多段恢复用):同一命名空间内按槽位升序合并 posting。
    ///
    /// 各段槽位互斥(一个全局槽位只属于一个段),故不存在同一槽位的 tf 叠并。
    pub(crate) fn merge_from(&mut self, other: InvertedIndex) {
        for (ns_id, docs) in other.docs {
            self.docs.entry(ns_id).or_default().extend(docs);
        }
        for (ns_id, terms) in other.terms {
            let bucket = self.terms.entry(ns_id).or_default();
            for (term, mut postings) in terms {
                match bucket.get_mut(&term) {
                    Some(existing) => {
                        existing.append(&mut postings);
                        existing.sort_by_key(|posting| posting.slot);
                    }
                    None => {
                        bucket.insert(term, postings);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_tf_and_doc_len() {
        let mut index = InvertedIndex::default();
        let ns = NsId::new(1);
        index.insert_text(SlotId::new(0), ns, "memory memory 记忆库", false);
        let postings = index.postings_of(ns, "memory").expect("memory");
        assert_eq!(postings.len(), 1);
        assert_eq!(postings[0].tf, 2);
        let docs = index.doc_entries(ns).expect("docs");
        assert_eq!(docs.get(&SlotId::new(0)), Some(&4));
        assert!(index.postings_of(ns, "记忆").is_some());
        assert!(index.postings_of(ns, "忆库").is_some());
    }

    #[test]
    fn namespaces_are_isolated() {
        let mut index = InvertedIndex::default();
        index.insert_text(SlotId::new(0), NsId::new(1), "alpha", false);
        index.insert_text(SlotId::new(1), NsId::new(2), "alpha", false);
        assert_eq!(
            index
                .postings_of(NsId::new(1), "alpha")
                .map(<[Posting]>::len),
            Some(1)
        );
        assert_eq!(
            index
                .postings_of(NsId::new(2), "alpha")
                .map(<[Posting]>::len),
            Some(1)
        );
        assert!(index.postings_of(NsId::new(3), "alpha").is_none());
    }

    #[test]
    fn empty_text_is_not_indexed() {
        let mut index = InvertedIndex::default();
        index.insert_text(SlotId::new(0), NsId::new(1), "   ", true);
        assert!(index.doc_entries(NsId::new(1)).is_none());
    }
}
