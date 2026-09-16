//! 写状态 [`WriterState`] 的字段定义、构造、ID 分配与 flush 记账。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::core::bitset::BitSet;
use crate::core::chunked::ChunkedVec;
use crate::core::error::{MnemeError, Result};
use crate::core::options::RelationKind;
use crate::core::sharded::ShardedMap;
use crate::core::types::{Key, NsId, RowId, SeqNo, SlotId};
use crate::memory::analysis::{BLOOM_INITIAL_CAPACITY, BloomSet, InvertedIndex, ZoneIndex};
use crate::memory::index::SegmentIndex;
use crate::memory::relation::Edge;
use crate::memory::table::view::ReaderView;
use crate::memory::table::write_op::WriteOp;

use super::slot::{AccessStat, SlotData};

/// 写路径的可变状态;容器字段均为 `Arc`,写入经 `Arc::make_mut` 触发 COW。
///
/// 实现 `Clone` 以便批量写入在失败时快照回滚:所有容器字段(`slots`/各索引/
/// `access`/`feedback_seen` 等)均为 `Arc`,`clone` 仅复制句柄,不深拷贝内容
/// (回滚后首次写入经 COW 触发一次拷贝,见 `Namespace::insert_batch`)。
#[derive(Clone)]
pub(crate) struct WriterState {
    pub(crate) slots: Arc<Vec<Arc<SlotData>>>,
    pub(crate) dead: Arc<BitSet>,
    pub(crate) key_index: ShardedMap<(NsId, Key), RowId>,
    pub(crate) text_index: ShardedMap<(NsId, u64), RowId>,
    pub(crate) versions: ShardedMap<RowId, Arc<Vec<SlotId>>>,
    pub(crate) latest: ShardedMap<RowId, SlotId>,
    pub(crate) out_edges: ShardedMap<RowId, Vec<Edge>>,
    pub(crate) in_edges: ShardedMap<RowId, Vec<Edge>>,
    pub(crate) access: ShardedMap<RowId, AccessStat>,
    pub(crate) seqno: SeqNo,
    /// 已物化(落盘进段)的槽位数量;`slots.len() - materialized_rows` 即未落盘行数。
    ///
    /// 槽位只追加,故用「总数 - 已物化数」维护未落盘计数,避免每次写事务 O(N) 扫描
    /// (WAL 阈值检查决定是否 flush,见 `persist::store::hook::maybe_flush`)。
    pub(crate) materialized_rows: usize,
    pub(crate) next_rowid: u64,
    pub(crate) next_ns_id: u32,
    pub(crate) ns_registry: Arc<HashMap<NsId, Arc<str>>>,
    pub(crate) ns_by_path: Arc<HashMap<Arc<str>, NsId>>,
    /// 自定义关系类型名称 → 编号(编号 ≥ [`RelationKind::FIRST_CUSTOM`],库内唯一)。
    pub(crate) rel_kinds: Arc<HashMap<Arc<str>, u16>>,
    /// 自定义关系类型编号 → 名称(与 `rel_kinds` 互逆)。
    pub(crate) rel_kind_names: Arc<HashMap<u16, Arc<str>>>,
    /// 下一个可分配的关系类型编号(永不复用,`u16::MAX` 表示空间耗尽)。
    pub(crate) next_rel_kind: u16,
    /// 各已落盘段的向量索引(多段架构;空 = 恒暴力扫描)。
    pub(crate) indexes: Arc<Vec<SegmentIndex>>,
    /// 与 `slots` 平行的"槽位 → 所属段编号";`None` = 尚未落盘的尾部槽位。
    pub(crate) slot_segment: ChunkedVec<Option<u32>>,
    /// 恢复期因损坏被隔离(内存跳过、文件原地保留)的段编号。
    ///
    /// 这些段仍在 MANIFEST 中,但内容不可用;compaction 计划必须排除它们,
    /// 否则会把损坏段当活跃段合并、移入 trash 并清除(FC-PERSIST-ERR-006)。
    pub(crate) unavailable_segments: Arc<HashSet<u32>>,
    /// 自上次 flush 以来访问计数增量(按 RowId;delta 区 Access 条目来源)。
    pub(crate) access_dirty: Arc<HashMap<RowId, u32>>,
    /// 自上次 flush 以来关系边变更(按 `(from, to, kind)`;delta 区 Relate/Unrelate 来源)。
    pub(crate) edge_dirty: Arc<HashSet<(RowId, RowId, u16)>>,
    /// 本进程累计物理回收的版本数(compaction;`history_horizon` 有限时增长)。
    pub(crate) reclaimed_versions: u64,
    /// 内存倒排索引(BM25;设计 04 §5.4)。
    pub(crate) inv: Arc<InvertedIndex>,
    /// 块级 zone map(过滤下推;设计 04 §5.2)。
    pub(crate) zones: Arc<ZoneIndex>,
    /// `key` 字段的布隆预筛(设计 04 §5.3)。
    pub(crate) key_bloom: Arc<BloomSet>,
    /// 索引与查询共用的分词停用词开关(`Tuning::stopwords`)。
    pub(crate) stopwords_enabled: bool,
    /// 可索引字段上限(`Tuning::field_dict_max`;重建 zone map 用)。
    pub(crate) index_fields_max: usize,
    /// 命名空间路径最大深度(`Limits::ns_depth`;首次写入时校验)。
    pub(crate) ns_depth_max: u16,
    /// bloom 目标误判率(`Tuning::bloom_fpp`;重建 bloom 用)。
    pub(crate) bloom_fpp: f32,
    /// 恢复期暂停索引增量维护(段载入完成后由磁盘索引或全量重建接管)。
    pub(crate) is_indexing_paused: bool,
    // 反馈幂等键(I27);L1 常驻内存,L5 随访问统计一并落盘。经 `Arc` COW,
    // 使批量写入快照(`WriterState::clone`)与回滚不深拷贝该集合。
    pub(crate) feedback_seen: Arc<HashSet<(RowId, u64)>>,
    // 当前写事务待持久化的操作;由 `write_tx` 在成功后交给 [`super::PersistHook`]。
    pub(crate) pending: Vec<WriteOp>,
    pub(crate) closed: bool,
}

impl WriterState {
    /// 构造空写状态(无段、无版本、`SeqNo`/`RowId`/`NsId` 水位从零开始)。
    pub(crate) fn new() -> Self {
        Self {
            slots: Arc::new(Vec::new()),
            dead: Arc::new(BitSet::default()),
            key_index: ShardedMap::new(),
            text_index: ShardedMap::new(),
            versions: ShardedMap::new(),
            latest: ShardedMap::new(),
            out_edges: ShardedMap::new(),
            in_edges: ShardedMap::new(),
            access: ShardedMap::new(),
            seqno: SeqNo::new(0),
            materialized_rows: 0,
            next_rowid: 0,
            next_ns_id: 1,
            ns_registry: Arc::new(HashMap::new()),
            ns_by_path: Arc::new(HashMap::new()),
            rel_kinds: Arc::new(HashMap::new()),
            rel_kind_names: Arc::new(HashMap::new()),
            next_rel_kind: RelationKind::FIRST_CUSTOM,
            indexes: Arc::new(Vec::new()),
            slot_segment: ChunkedVec::new(),
            unavailable_segments: Arc::new(HashSet::new()),
            access_dirty: Arc::new(HashMap::new()),
            edge_dirty: Arc::new(HashSet::new()),
            reclaimed_versions: 0,
            // 加速结构的兜底初值:生产入口(Builder/open)都会用配置覆写;
            // 16 / 0.01 与 `Tuning::default()` 保持同口径,仅供 `Default` 构造。
            inv: Arc::new(InvertedIndex::default()),
            zones: Arc::new(ZoneIndex::new(16)),
            key_bloom: Arc::new(BloomSet::new(BLOOM_INITIAL_CAPACITY, 0.01)),
            stopwords_enabled: true,
            index_fields_max: 16,
            ns_depth_max: 32,
            bloom_fpp: 0.01,
            is_indexing_paused: false,
            feedback_seen: Arc::new(HashSet::new()),
            pending: Vec::new(),
            closed: false,
        }
    }

    /// 分配下一个全局单调 `SeqNo`。
    ///
    /// # Errors
    /// `SeqNo` 空间耗尽时返回 [`MnemeError::IdExhausted`]
    /// (FC-PERSIST-INV-020),绝不回绕复用。
    pub(crate) fn alloc_seqno(&mut self) -> Result<SeqNo> {
        let next = self
            .seqno
            .get()
            .checked_add(1)
            .ok_or(MnemeError::IdExhausted { kind: "seqno" })?;
        self.seqno = SeqNo::new(next);
        Ok(self.seqno)
    }

    /// 分配下一个稳定 `RowId`。
    ///
    /// # Errors
    /// `RowId` 空间耗尽时返回 [`MnemeError::IdExhausted`]
    /// (FC-PERSIST-INV-020),绝不回绕复用。
    pub(crate) fn alloc_rowid(&mut self) -> Result<RowId> {
        let rowid = RowId::new(self.next_rowid);
        self.next_rowid = self
            .next_rowid
            .checked_add(1)
            .ok_or(MnemeError::IdExhausted { kind: "rowid" })?;
        Ok(rowid)
    }

    /// 返回命名空间路径对应的 `NsId`(不存在则 `None`)。
    pub(crate) fn ns_id_of(&self, path: &str) -> Option<NsId> {
        self.ns_by_path.get(path).copied()
    }

    /// 注册命名空间路径(已存在则返回既有 `NsId`),分配单调 `NsId`。
    ///
    /// 首次登记时校验路径深度 ≤ `Limits.ns_depth` 且不含控制字符:
    /// `namespace()` 不返回 `Result`,错误在首次写入时以 `Config` 报告
    /// (FC-LIFE-POST-005)。
    ///
    /// # Errors
    /// 路径过深或含控制字符时返回 [`MnemeError::Config`]。
    pub(crate) fn register_ns(&mut self, path: &str) -> Result<NsId> {
        if let Some(id) = self.ns_by_path.get(path) {
            return Ok(*id);
        }
        let depth = if path.is_empty() {
            0
        } else {
            path.split('/').count()
        };
        if depth > usize::from(self.ns_depth_max) {
            return Err(MnemeError::Config {
                reason: "命名空间路径深度超限",
            });
        }
        if path
            .split('/')
            .any(|segment| segment.chars().any(char::is_control))
        {
            return Err(MnemeError::Config {
                reason: "命名空间路径含控制字符",
            });
        }
        let next_ns_id = self
            .next_ns_id
            .checked_add(1)
            .ok_or(MnemeError::IdExhausted { kind: "ns_id" })?;
        let id = NsId::new(self.next_ns_id);
        self.next_ns_id = next_ns_id;
        let path: Arc<str> = Arc::from(path);
        Arc::make_mut(&mut self.ns_registry).insert(id, path.clone());
        Arc::make_mut(&mut self.ns_by_path).insert(path.clone(), id);
        let seqno = self.alloc_seqno()?;
        self.pending.push(WriteOp::NsRegister {
            ns_id: id.get(),
            path,
            seqno,
        });
        Ok(id)
    }

    /// 注销命名空间(移除注册表与路径映射并记录 WAL 帧);`NsId` 水位不回退、永不复用。
    ///
    /// # Errors
    /// `SeqNo` 空间耗尽时返回 [`MnemeError::IdExhausted`]。
    pub(crate) fn unregister_ns(&mut self, ns_id: NsId) -> Result<()> {
        if let Some(path) = Arc::make_mut(&mut self.ns_registry).remove(&ns_id) {
            Arc::make_mut(&mut self.ns_by_path).remove(&path);
        }
        let seqno = self.alloc_seqno()?;
        self.pending.push(WriteOp::NsUnregister {
            ns_id: ns_id.get(),
            seqno,
        });
        Ok(())
    }
}

impl WriterState {
    /// 清空自上次 flush 以来的访问/关系 delta 标记(段提交成功后调用)。
    pub(crate) fn clear_flush_dirty(&mut self) {
        self.access_dirty = Arc::new(HashMap::new());
        self.edge_dirty = Arc::new(HashSet::new());
    }

    /// 记录一次访问计数增量(delta 区 `Access` 条目来源)。
    pub(crate) fn mark_access_dirty(&mut self, rowid: RowId) {
        self.mark_access_dirty_by(rowid, 1);
    }

    /// 记录 `delta` 次访问计数增量(读路径攒批合并后调用)。
    pub(crate) fn mark_access_dirty_by(&mut self, rowid: RowId, delta: u32) {
        if delta == 0 {
            return;
        }
        let entry = Arc::make_mut(&mut self.access_dirty)
            .entry(rowid)
            .or_insert(0);
        *entry = entry.saturating_add(delta);
    }

    /// 尚未落盘的槽位数量(槽位只追加,总数减已物化数)。
    pub(crate) fn unpersisted_count(&self) -> usize {
        self.slots.len().saturating_sub(self.materialized_rows)
    }

    /// 登记 `count` 个槽位已随段落盘(flush 安装段后调用)。
    pub(crate) fn note_materialized(&mut self, count: usize) {
        self.materialized_rows = self.materialized_rows.saturating_add(count);
    }

    /// 按 `slot_segment` 归属重算已物化行数(恢复完成后调用一次)。
    ///
    /// 恢复路径的槽位要么来自已落盘段(`Some`),要么来自 WAL 重放(`None`,未落盘);
    /// 计数不能让 WAL 阈值误判,故以归属位图为准校准。
    pub(crate) fn recount_materialized(&mut self) {
        let unpersisted = self
            .slot_segment
            .iter()
            .filter(|entry| entry.is_none())
            .count();
        self.materialized_rows = self.slots.len().saturating_sub(unpersisted);
    }

    /// 返回尚未落盘的槽位下标(升序;delta 段物化输入)。
    pub(crate) fn unpersisted_slots(&self) -> Vec<usize> {
        self.slot_segment
            .iter()
            .enumerate()
            .filter_map(|(index, segment)| segment.is_none().then_some(index))
            .collect()
    }

    /// 下一个内存段编号(纯内存库建段用;持久段编号来自 MANIFEST)。
    ///
    /// 取现存内存段编号最大值 + 1:段编号在单个视图内唯一即可(与持久库同口径,
    /// 供段级 `alive` 位图缓存键 `(段号, NsId)` 使用),不要求全局单调。
    pub(crate) fn next_memory_segment_id(&self) -> u32 {
        self.indexes
            .iter()
            .map(|segment| segment.segment_id.saturating_add(1))
            .max()
            .unwrap_or(0)
    }

    /// 清空自上次 flush 以来关系变更标记(全量重写关系表后调用)。
    pub(crate) fn clear_edge_dirty(&mut self) {
        self.edge_dirty = Arc::new(HashSet::new());
    }

    /// 记录本轮 compaction 回收的版本数。
    pub(crate) fn note_reclaimed(&mut self, count: usize) {
        self.reclaimed_versions = self.reclaimed_versions.saturating_add(count as u64);
    }
}

impl WriterState {
    /// 构造一份不可变读视图(仅克隆 `Arc` 句柄)。
    pub(crate) fn snapshot(&self) -> ReaderView {
        ReaderView {
            slots: Arc::clone(&self.slots),
            dead: Arc::clone(&self.dead),
            key_index: self.key_index.clone(),
            versions: self.versions.clone(),
            latest: self.latest.clone(),
            out_edges: self.out_edges.clone(),
            in_edges: self.in_edges.clone(),
            access: self.access.clone(),
            ns_registry: Arc::clone(&self.ns_registry),
            ns_by_path: Arc::clone(&self.ns_by_path),
            indexes: Arc::clone(&self.indexes),
            slot_segment: self.slot_segment.clone(),
            reclaimed_versions: self.reclaimed_versions,
            inv: Arc::clone(&self.inv),
            zones: Arc::clone(&self.zones),
            key_bloom: Arc::clone(&self.key_bloom),
            seqno: self.seqno,
            closed: self.closed,
            // 缓存随视图生命周期:每个快照独立空缓存,写事务发布会替换视图
            // (FC-QUERY-POST-008)。
            plan_cache: Mutex::new(HashMap::new()),
            segment_alive_cache: Mutex::new(HashMap::new()),
        }
    }
}
