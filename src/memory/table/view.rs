//! 不可变读视图(`table/view.rs`)。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::core::bitset::BitSet;
use crate::core::chunked::ChunkedVec;
use crate::core::sharded::ShardedMap;
use crate::core::types::{Key, NsId, RowId, SeqNo, SlotId};
use crate::memory::analysis::{BloomSet, InvertedIndex, ZoneIndex};
use crate::memory::index::SegmentIndex;
use crate::memory::relation::Edge;

use super::AccessStat;
use super::state::SlotData;

/// 无过滤、命名空间内无 TTL 行的查询计划缓存条目(`FC-QUERY-POST-008`)。
///
/// 只挂在不可变 [`ReaderView`] 上:写事务发布会替换视图,缓存随旧快照自然
/// 失效,绝不跨视图复用;含 `expires_at` 行的命名空间不写缓存(过期实时反映)。
pub(crate) struct CachedPlan {
    /// 可见候选槽位(按槽位升序;与逐行三值求值全等)。
    pub(crate) candidates: Arc<Vec<u32>>,
    /// 候选槽位位图(BM25 通道共享)。
    pub(crate) bits: Arc<BitSet>,
    /// 选择性 = 候选数 / 命名空间活行数(无活行时为 0)。
    pub(crate) selectivity: f32,
}

/// 不可变读视图:读者克隆后无锁扫描。
pub(crate) struct ReaderView {
    pub(crate) slots: Arc<Vec<Arc<SlotData>>>,
    pub(crate) dead: Arc<BitSet>,
    pub(crate) key_index: ShardedMap<(NsId, Key), RowId>,
    pub(crate) versions: ShardedMap<RowId, Arc<Vec<SlotId>>>,
    pub(crate) latest: ShardedMap<RowId, SlotId>,
    pub(crate) out_edges: ShardedMap<RowId, Vec<Edge>>,
    pub(crate) in_edges: ShardedMap<RowId, Vec<Edge>>,
    pub(crate) access: ShardedMap<RowId, AccessStat>,
    pub(crate) ns_registry: Arc<HashMap<NsId, Arc<str>>>,
    /// 命名空间路径 → `NsId`(与 `ns_registry` 互逆;查询期 $O(1)$ 解析)。
    pub(crate) ns_by_path: Arc<HashMap<Arc<str>, NsId>>,
    /// 各已落盘段的向量索引(与视图一同快照,保证快照一致;空 = 恒暴力)。
    pub(crate) indexes: Arc<Vec<SegmentIndex>>,
    /// 与 `slots` 平行的"槽位 → 所属段编号"(`None` = 未落盘尾部)。
    pub(crate) slot_segment: ChunkedVec<Option<u32>>,
    /// 本进程累计物理回收的版本数(compaction)。
    pub(crate) reclaimed_versions: u64,
    /// 内存倒排索引(BM25 两遍统计;与视图一同快照)。
    pub(crate) inv: Arc<InvertedIndex>,
    /// 块级 zone map(过滤下推)。
    pub(crate) zones: Arc<ZoneIndex>,
    /// `key` 字段的布隆预筛。
    pub(crate) key_bloom: Arc<BloomSet>,
    pub(crate) seqno: SeqNo,
    /// 库是否已关闭(关闭后读写返回 `Closed`)。
    pub(crate) closed: bool,
    /// 命名空间查询计划缓存(键 `NsId`;仅无过滤且无 TTL 行时写入)。
    pub(crate) plan_cache: Mutex<HashMap<NsId, Arc<CachedPlan>>>,
    /// 段 `alive` 位图缓存(键 `(段号, NsId)`;仅无过滤且无 TTL 行时写入)。
    pub(crate) segment_alive_cache: Mutex<HashMap<(u32, NsId), Arc<BitSet>>>,
}

impl ReaderView {
    /// 返回 `rowid` 当前可见的物理槽位(被遮蔽/墓碑则为 `None`)。
    pub(crate) fn live_slot(&self, rowid: RowId) -> Option<SlotId> {
        let slot = *self.latest.get(&rowid)?;
        if self.dead.get(slot.get() as usize) {
            return None;
        }
        let slot_data = self.slots.get(slot.get() as usize)?;
        if slot_data.deleted {
            return None;
        }
        Some(slot)
    }

    /// 按 `(ns_id, key)` 解析稳定 `RowId`。
    pub(crate) fn rowid_of_key(&self, ns_id: NsId, key: &Key) -> Option<RowId> {
        self.key_index.get(&(ns_id, key.clone())).copied()
    }

    /// 取命名空间计划缓存(命中返回 `Arc`,零拷贝共享;`None` = 未缓存)。
    pub(crate) fn cached_plan(&self, ns_id: NsId) -> Option<Arc<CachedPlan>> {
        // reason: 缓存中毒只影响缓存可用性、不影响查询正确性;取回内部值继续,
        // 绝不因缓存基础设施 panic(FC-QUERY-POST-008 的语义透明)。
        let cache = self
            .plan_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.get(&ns_id).cloned()
    }

    /// 写入命名空间计划缓存(已存在时不覆盖,保证同内容幂等)。
    pub(crate) fn store_plan(&self, ns_id: NsId, plan: Arc<CachedPlan>) {
        let mut cache = self
            .plan_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.entry(ns_id).or_insert(plan);
    }

    /// 取段 `alive` 位图缓存(命中返回 `Arc`,零拷贝共享)。
    pub(crate) fn cached_segment_alive(&self, segment_id: u32, ns_id: NsId) -> Option<Arc<BitSet>> {
        let cache = self
            .segment_alive_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.get(&(segment_id, ns_id)).cloned()
    }

    /// 写入段 `alive` 位图缓存(已存在时不覆盖)。
    pub(crate) fn store_segment_alive(&self, segment_id: u32, ns_id: NsId, alive: Arc<BitSet>) {
        let mut cache = self
            .segment_alive_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.entry((segment_id, ns_id)).or_insert(alive);
    }
}
