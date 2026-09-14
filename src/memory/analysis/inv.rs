//! 内存倒排索引(`analysis/inv.rs`,设计 04 §5.4、06 §3.4)。
//!
//! 结构:`NsId → (term 分片 → [(SlotId, tf)])` 与 `NsId → (槽位分片 → 文档词数)`。
//! 分片经 `Arc` 共享、写时只对命中分片做写时复制(COW):`postings` 以
//! `Arc<Vec<Posting>>` 独立共享,追加新文档时只深拷命中的词条 postings 与该分片
//! 的浅层条目表,不再整表深拷。写路径在
//! [`WriterState::commit_version`](super::super::table::WriterState) 时增量插入;
//! 旧版本槽位保留在 postings 中,由查询期按视图可见性过滤(支持 `as_of`)。
//!
//! 倒排按命名空间分桶,BM25 统计天然满足 NS 隔离(I21)。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::core::text::tokenize;
use crate::core::types::{NsId, SlotId};

/// 词条分片数(按 term 哈希)。写一个文档触及的词条分散在各分片,单事务的
/// 浅层条目拷贝上界为「全部词条表」;分片使未触及分片保持共享。
const TERM_SHARDS: usize = 64;

/// 文档分片数(按槽位块号,每 1024 槽位一块)。批量追加集中在尾部块,只触及
/// 少数分片,避免每事务深拷整张文档表。
const DOC_SHARDS: usize = 64;

/// 一条 posting:物理槽位与词频。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Posting {
    /// 物理槽位。
    pub(crate) slot: SlotId,
    /// 词在该文档中的出现次数。
    pub(crate) tf: u32,
}

/// 词条分片:`term → postings`;postings 独立 `Arc` 共享。
type TermShard = HashMap<Arc<str>, Arc<Vec<Posting>>>;

/// 文档分片:`slot → 文档词数`。
type DocShard = HashMap<SlotId, u32>;

/// 单命名空间的倒排桶(分片表;`Clone` 只复制分片句柄)。
#[derive(Debug, Clone)]
struct NsBucket {
    /// 词条分片;每个 postings 独立 `Arc` 使写只深拷命中的词条。
    term_shards: Vec<Arc<TermShard>>,
    /// 文档词数分片。
    doc_shards: Vec<Arc<DocShard>>,
}

impl NsBucket {
    /// 空桶(固定数量的空分片)。
    fn new() -> Self {
        Self {
            term_shards: (0..TERM_SHARDS).map(|_| Arc::new(HashMap::new())).collect(),
            doc_shards: (0..DOC_SHARDS).map(|_| Arc::new(HashMap::new())).collect(),
        }
    }

    /// 词条所属分片下标(确定性哈希,仅用于分散)。
    fn term_shard_index(term: &str) -> usize {
        let mut hasher = std::hash::DefaultHasher::new();
        term.hash(&mut hasher);
        (hasher.finish() as usize) % TERM_SHARDS
    }

    /// 槽位所属文档分片下标(按 1024 槽位块号)。
    fn doc_shard_index(slot: SlotId) -> usize {
        (slot.get() as usize / 1024) % DOC_SHARDS
    }

    /// 登记文档词数(覆盖写,后写者为准)。
    fn insert_doc(&mut self, slot: SlotId, doc_len: u32) {
        Arc::make_mut(&mut self.doc_shards[Self::doc_shard_index(slot)]).insert(slot, doc_len);
    }

    /// 追加一条词出现;同一槽位连续同词累计 tf。
    fn insert_posting(&mut self, slot: SlotId, term: &str) {
        let shard = Arc::make_mut(&mut self.term_shards[Self::term_shard_index(term)]);
        match shard.get_mut(term) {
            Some(postings) => push_posting(Arc::make_mut(postings), slot),
            None => {
                let mut postings = Vec::new();
                push_posting(&mut postings, slot);
                shard.insert(Arc::from(term), Arc::new(postings));
            }
        }
    }
}

/// 追加一条词出现:同槽位连续同词累计 tf,否则新条目。
fn push_posting(postings: &mut Vec<Posting>, slot: SlotId) {
    match postings.last_mut() {
        Some(last) if last.slot == slot => last.tf += 1,
        _ => postings.push(Posting { slot, tf: 1 }),
    }
}

/// 内存倒排索引。
#[derive(Debug, Default, Clone)]
pub(crate) struct InvertedIndex {
    /// 命名空间 → 倒排桶;顶层写时只浅拷 `Arc` 句柄。
    buckets: HashMap<NsId, Arc<NsBucket>>,
}

impl InvertedIndex {
    /// 取(或建立)命名空间桶的可变引用。
    fn bucket_mut(&mut self, ns_id: NsId) -> &mut NsBucket {
        let bucket = self
            .buckets
            .entry(ns_id)
            .or_insert_with(|| Arc::new(NsBucket::new()));
        Arc::make_mut(bucket)
    }

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
        let bucket = self.bucket_mut(ns_id);
        bucket.insert_doc(slot, tokens.len() as u32);
        for token in tokens {
            bucket.insert_posting(slot, &token);
        }
    }

    /// 迭代命名空间内"槽位 → 文档词数"(未按可见性过滤)。
    pub(crate) fn docs_of(&self, ns_id: NsId) -> impl Iterator<Item = (&SlotId, &u32)> + '_ {
        self.buckets
            .get(&ns_id)
            .into_iter()
            .flat_map(|bucket| bucket.doc_shards.iter())
            .flat_map(|shard| shard.iter())
    }

    /// 单文档词数;该槽位无文本记录时返回 `None`。
    pub(crate) fn doc_len(&self, ns_id: NsId, slot: SlotId) -> Option<u32> {
        let bucket = self.buckets.get(&ns_id)?;
        let shard = bucket.doc_shards.get(NsBucket::doc_shard_index(slot))?;
        shard.get(&slot).copied()
    }

    /// 返回某命名空间中指定词的 postings。
    pub(crate) fn postings_of(&self, ns_id: NsId, term: &str) -> Option<&[Posting]> {
        let bucket = self.buckets.get(&ns_id)?;
        let shard = bucket.term_shards.get(NsBucket::term_shard_index(term))?;
        shard.get(term).map(|postings| postings.as_slice())
    }

    /// 已登记的命名空间(落盘编码用)。
    pub(crate) fn ns_ids(&self) -> impl Iterator<Item = NsId> + '_ {
        self.buckets.keys().copied()
    }

    /// 迭代某命名空间的全部词条与 postings(落盘编码用)。
    pub(crate) fn terms_of(
        &self,
        ns_id: NsId,
    ) -> impl Iterator<Item = (&Arc<str>, &[Posting])> + '_ {
        self.buckets
            .get(&ns_id)
            .into_iter()
            .flat_map(|bucket| bucket.term_shards.iter())
            .flat_map(|shard| shard.iter())
            .map(|(term, postings)| (term, postings.as_slice()))
    }

    /// 直接插入一条已聚合的 posting 列表(倒排落盘解码用)。
    ///
    /// 调用方保证 postings 按 `SlotId` 升序且同一槽位已聚合 tf。
    pub(crate) fn insert_term(&mut self, ns_id: NsId, term: Arc<str>, postings: Vec<Posting>) {
        let bucket = self.bucket_mut(ns_id);
        let shard = Arc::make_mut(&mut bucket.term_shards[NsBucket::term_shard_index(&term)]);
        shard.insert(term, Arc::new(postings));
    }

    /// 登记一个文档词数(倒排落盘解码用)。
    pub(crate) fn insert_doc(&mut self, ns_id: NsId, slot: SlotId, doc_len: u32) {
        self.bucket_mut(ns_id).insert_doc(slot, doc_len);
    }

    /// 合并另一倒排索引(多段恢复用):同一命名空间内按槽位升序合并 posting。
    ///
    /// 各段槽位互斥(一个全局槽位只属于一个段),故不存在同一槽位的 tf 叠并。
    pub(crate) fn merge_from(&mut self, other: InvertedIndex) {
        for (ns_id, other_bucket) in other.buckets {
            let bucket = Arc::make_mut(
                self.buckets
                    .entry(ns_id)
                    .or_insert_with(|| Arc::new(NsBucket::new())),
            );
            for (index, other_shard) in other_bucket.doc_shards.iter().enumerate() {
                Arc::make_mut(&mut bucket.doc_shards[index])
                    .extend(other_shard.iter().map(|(slot, doc_len)| (*slot, *doc_len)));
            }
            for (index, other_shard) in other_bucket.term_shards.iter().enumerate() {
                for (term, postings) in other_shard.iter() {
                    let target = &mut bucket.term_shards[index];
                    let shard = Arc::make_mut(target);
                    match shard.get_mut(term.as_ref()) {
                        Some(existing) => {
                            let merged = Arc::make_mut(existing);
                            merged.extend_from_slice(postings);
                            merged.sort_by_key(|posting| posting.slot);
                        }
                        None => {
                            shard.insert(Arc::clone(term), Arc::clone(postings));
                        }
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
        assert_eq!(index.doc_len(ns, SlotId::new(0)), Some(4));
        assert_eq!(index.docs_of(ns).count(), 1);
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
        assert_eq!(index.docs_of(NsId::new(1)).count(), 0);
    }

    /// 分片结构下克隆与后续写入互不泄漏(块级/分片级 COW)。
    #[test]
    fn clone_and_merge_are_copy_on_write() {
        let mut index = InvertedIndex::default();
        index.insert_text(SlotId::new(0), NsId::new(1), "alpha beta", false);
        let snapshot = index.clone();
        index.insert_text(SlotId::new(2000), NsId::new(1), "alpha gamma", false);
        assert_eq!(index.postings_of(NsId::new(1), "alpha").unwrap().len(), 2);
        assert_eq!(
            snapshot.postings_of(NsId::new(1), "alpha").unwrap().len(),
            1,
            "快照不得被后续写入改变"
        );
        // merge_from:postings 合并后按槽位升序。
        let mut merged = InvertedIndex::default();
        merged.insert_text(SlotId::new(1), NsId::new(1), "alpha", false);
        let mut other = InvertedIndex::default();
        other.insert_text(SlotId::new(0), NsId::new(1), "alpha", false);
        merged.merge_from(other);
        let postings = merged.postings_of(NsId::new(1), "alpha").expect("alpha");
        assert_eq!(
            postings.iter().map(|p| p.slot.get()).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }
}
