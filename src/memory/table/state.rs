//! 物理槽位与写状态(`table/state.rs`)。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::core::bitset::BitSet;
use crate::core::chunked::ChunkedVec;
use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::options::{RelationKind, VectorFormat};
use crate::core::sharded::ShardedMap;
use crate::core::types::{Key, NsId, RowId, SeqNo, SlotId};
use crate::memory::analysis::{BLOOM_INITIAL_CAPACITY, BloomSet, InvertedIndex, ZoneIndex};
use crate::memory::index::{SegmentIndex, VectorIndex};
use crate::memory::lazy::VectorStorage;
use crate::memory::relation::Edge;

use super::view::ReaderView;
use super::write_op::WriteOp;

/// 单条记录的访问统计(内存累积;L5 起落盘)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccessStat {
    /// 最近一次访问时刻(Unix 毫秒)。
    pub last_access_ms: i64,
    /// 累计访问次数。
    pub access_count: u32,
}

/// 自定义关系类型名称最大字节数(防注册表被超长名称膨胀)。
const REL_KIND_NAME_MAX: usize = 128;

/// 内置关系类型的名称解析(与 [`RelationKind`] 常量同名,便于宿主用字符串配置)。
fn builtin_relation_kind(name: &str) -> Option<RelationKind> {
    match name {
        "derived_from" => Some(RelationKind::DERIVED_FROM),
        "supports" => Some(RelationKind::SUPPORTS),
        "contradicts" => Some(RelationKind::CONTRADICTS),
        "related" => Some(RelationKind::RELATED),
        _ => None,
    }
}

/// 把槽位下标映射为 `SlotId`;超出 `u32::MAX` 时返回结构化错误,绝不静默饱和
/// (FC-MEM-INV-004)。
pub(crate) fn slot_id_for(len: usize) -> Result<SlotId> {
    u32::try_from(len)
        .map(SlotId::new)
        .map_err(|_| MnemeError::LimitExceeded {
            field: "slots",
            limit: u32::MAX as usize,
            got: len,
        })
}

/// 一个物理版本(不可变,`Arc` 共享)。下标即 `SlotId`。
#[derive(Debug, Clone)]
pub(crate) struct SlotData {
    pub(crate) rowid: RowId,
    pub(crate) ns_id: NsId,
    pub(crate) ns_path: Arc<str>,
    pub(crate) seqno: SeqNo,
    pub(crate) key: Option<Key>,
    /// 向量(自有或段句柄惰性;统一经 `Deref` 读为 `&[f32]`)。
    pub(crate) vector: Arc<VectorStorage>,
    pub(crate) norm_sq: f32,
    pub(crate) text: Option<Arc<str>>,
    pub(crate) text_hash: Option<u64>,
    pub(crate) meta: Meta,
    pub(crate) created_at: i64,
    pub(crate) expires_at: Option<i64>,
    pub(crate) importance: f32,
    pub(crate) confidence: f32,
    pub(crate) valid_from: i64,
    pub(crate) valid_to: Option<i64>,
    pub(crate) provenance: Option<Meta>,
    pub(crate) tx_ms: i64,
    pub(crate) deleted: bool,
}

impl SlotData {
    /// 在当前时刻 `now_ms` 是否可见(未墓碑、未逻辑过期)。
    pub(crate) fn is_live(&self, now_ms: i64) -> bool {
        !self.deleted && self.expires_at.is_none_or(|expires| expires > now_ms)
    }

    /// 可见性判定,可跳过 TTL 逐行比较(块级 `ttl_map` 已证明整块未过期时)。
    pub(crate) fn is_live_with_ttl(&self, now_ms: i64, check_ttl: bool) -> bool {
        if self.deleted {
            return false;
        }
        !check_ttl || self.expires_at.is_none_or(|expires| expires > now_ms)
    }
}

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

    /// 注册/解析一个自定义关系类型名称(编号单调分配、永不复用)。
    ///
    /// 内置名(`derived_from`/`supports`/`contradicts`/`related`)解析为内置编号;
    /// 同名重复调用幂等返回既有编号。名称须非空、≤ 128 字节、不含控制字符。
    ///
    /// # Errors
    /// - 名称为空/超长/含控制字符 → [`MnemeError::Config`];
    /// - 编号空间耗尽(`next_rel_kind == u16::MAX`)→ [`MnemeError::TooLarge`];
    /// - `SeqNo` 空间耗尽 → [`MnemeError::IdExhausted`]。
    pub(crate) fn register_relation_kind(&mut self, name: &str) -> Result<RelationKind> {
        if let Some(kind) = builtin_relation_kind(name) {
            return Ok(kind);
        }
        if let Some(&kind) = self.rel_kinds.get(name) {
            return Ok(RelationKind(kind));
        }
        if name.is_empty() || name.len() > REL_KIND_NAME_MAX || name.chars().any(char::is_control) {
            return Err(MnemeError::Config {
                reason: "关系类型名须非空、≤128 字节且不含控制字符",
            });
        }
        if self.next_rel_kind == u16::MAX {
            return Err(MnemeError::TooLarge {
                field: "rel_kind",
                limit: u16::MAX as usize,
                got: self.next_rel_kind as usize,
            });
        }
        let kind = self.next_rel_kind;
        self.next_rel_kind += 1;
        let name: Arc<str> = Arc::from(name);
        Arc::make_mut(&mut self.rel_kinds).insert(Arc::clone(&name), kind);
        Arc::make_mut(&mut self.rel_kind_names).insert(kind, Arc::clone(&name));
        let seqno = self.alloc_seqno()?;
        self.pending
            .push(WriteOp::RelKindRegister { kind, name, seqno });
        Ok(RelationKind(kind))
    }

    /// 恢复期登记一个自定义关系类型(来自 MANIFEST 或 WAL 帧)。
    ///
    /// 编号必须 ≥ [`RelationKind::FIRST_CUSTOM`];同名不同号或同号不同名一律
    /// [`MnemeError::Corrupted`],绝不静默改写注册表;水位推进失败同样拒绝
    /// (FC-MODEL-POST-008、FC-PERSIST-ERR-012)。
    ///
    /// # Errors
    /// 见上。
    pub(crate) fn register_recovered_rel_kind(&mut self, kind: u16, name: Arc<str>) -> Result<()> {
        let corrupt = |reason: &str| MnemeError::Corrupted {
            segment: None,
            reason: format!("rel_kind 注册表:{reason}"),
        };
        if kind < RelationKind::FIRST_CUSTOM || name.is_empty() {
            return Err(corrupt("编号低于自定义起点或名称为空"));
        }
        if let Some(existing) = self.rel_kinds.get(&name).copied()
            && existing != kind
        {
            return Err(corrupt("同名不同编号"));
        }
        if let Some(existing) = self.rel_kind_names.get(&kind)
            && existing.as_ref() != name.as_ref()
        {
            return Err(corrupt("同编号不同名"));
        }
        Arc::make_mut(&mut self.rel_kinds).insert(Arc::clone(&name), kind);
        Arc::make_mut(&mut self.rel_kind_names).insert(kind, name);
        let next = kind
            .checked_add(1)
            .ok_or_else(|| corrupt("编号已达 u16::MAX"))?;
        if next > self.next_rel_kind {
            self.next_rel_kind = next;
        }
        Ok(())
    }

    /// 建立/更新关系边并记录 WAL 操作。
    ///
    /// # Errors
    /// `SeqNo` 空间耗尽时返回 [`MnemeError::IdExhausted`]。
    pub(crate) fn relate_edge(&mut self, edge: Edge) -> Result<()> {
        crate::memory::relation::upsert_edge_sharded(&mut self.out_edges, edge.clone());
        crate::memory::relation::upsert_edge_sharded(&mut self.in_edges, edge.clone());
        Arc::make_mut(&mut self.edge_dirty).insert((edge.from, edge.to, edge.kind.0));
        let seqno = self.alloc_seqno()?;
        self.pending.push(WriteOp::Relate {
            from: edge.from,
            to: edge.to,
            kind: edge.kind.0,
            weight: edge.weight,
            meta: edge.metadata,
            seqno,
        });
        Ok(())
    }

    /// 删除关系边并记录 WAL 操作;未命中返回 `false` 且不消耗序号。
    ///
    /// # Errors
    /// 命中边且 `SeqNo` 空间耗尽时返回 [`MnemeError::IdExhausted`]。
    pub(crate) fn unrelate_edge(
        &mut self,
        from: RowId,
        to: RowId,
        kind: RelationKind,
    ) -> Result<bool> {
        let removed =
            crate::memory::relation::remove_edge_sharded(&mut self.out_edges, from, to, kind);
        crate::memory::relation::remove_edge_sharded(&mut self.in_edges, to, from, kind);
        if removed {
            Arc::make_mut(&mut self.edge_dirty).insert((from, to, kind.0));
            let seqno = self.alloc_seqno()?;
            self.pending.push(WriteOp::Unrelate {
                from,
                to,
                kind: kind.0,
                seqno,
            });
        }
        Ok(removed)
    }

    /// 把某 `RowId` 的当前最新版本标记为不可见(被遮蔽/删除)。
    pub(crate) fn hide_latest(&mut self, rowid: RowId) {
        if let Some(slot) = self.latest.get(&rowid).copied() {
            Arc::make_mut(&mut self.dead).set(slot.get() as usize);
        }
    }

    /// 记录一个物理版本到版本链,并更新 `latest` / `key_index` / `text_index`。
    ///
    /// 若该 `RowId` 的上一版本带 key 且与新版本 key 不同(如 `supersede`/`merge`
    /// 改变 key),先移除旧 `key_index` 项,避免悬挂映射使 `get`/`check` 失准。
    pub(crate) fn link_version(&mut self, rowid: RowId, slot: SlotId) {
        if let Some((ns_id, old_key)) = self.previous_key(rowid) {
            let new_key = self.slots[slot.get() as usize].key.as_ref();
            if new_key != Some(&old_key) {
                self.key_index.remove(&(ns_id, old_key));
            }
        }
        let chain = self.versions.get_or_insert_default(rowid);
        Arc::make_mut(chain).push(slot);
        self.latest.insert(rowid, slot);
        let slot_data = Arc::clone(&self.slots[slot.get() as usize]);
        if let Some(key) = &slot_data.key {
            self.key_index.insert((slot_data.ns_id, key.clone()), rowid);
        }
        if let Some(hash) = slot_data.text_hash {
            self.text_index.insert((slot_data.ns_id, hash), rowid);
        }
    }

    /// 遮蔽前 `rowid` 当前最新版本的 `(ns_id, key)`(无 key 或不存在时 `None`)。
    fn previous_key(&self, rowid: RowId) -> Option<(NsId, Key)> {
        let previous = self.latest.get(&rowid).copied()?;
        let slot_data = &self.slots[previous.get() as usize];
        slot_data
            .key
            .as_ref()
            .map(|key| (slot_data.ns_id, key.clone()))
    }

    /// 提交一个新物理版本:遮蔽旧版本、追加、登记版本链。
    ///
    /// 容量校验与 key 占用校验都在遮蔽旧版本**之前**完成:槽位溢出(`u32::MAX`)
    /// 或新版本 key 已被另一可见记录占用时返回结构化错误且不改动旧版本,消除
    /// 「已遮蔽但无新版本」的半写(FC-MEM-PRE-002 零部分写入、FC-MEM-POST-007)。
    pub(crate) fn commit_version(&mut self, rowid: RowId, slot_data: SlotData) -> Result<SlotId> {
        let slot = slot_id_for(self.slots.len())?;
        ensure_key_available(self, &slot_data)?;
        let deleted = slot_data.deleted;
        let seqno = slot_data.seqno;
        let tx_ms = slot_data.tx_ms;
        self.hide_latest(rowid);
        let arc = Arc::new(slot_data);
        Arc::make_mut(&mut self.slots).push(Arc::clone(&arc));
        self.slot_segment.push(None);
        self.link_version(rowid, slot);
        if !deleted && !self.is_indexing_paused {
            self.index_observe(slot, &arc);
        }
        // 记录待持久化操作:墓碑落 `DeleteRow`,其余落完整新版本 `Insert`。
        if deleted {
            self.pending.push(WriteOp::DeleteRow {
                rowid,
                seqno,
                tx_ms,
            });
        } else {
            self.pending.push(WriteOp::Insert { slot: arc });
        }
        Ok(slot)
    }

    /// 恢复专用提交:只建立版本链与槽位,不记 WAL 操作、不动索引增量。
    ///
    /// 段数据在写入时已落 WAL/段,恢复期重复记录 pending 纯属开销;key 占用校验
    /// 保留(损坏段可能携带重复 key,绝不静默覆盖他人 `key_index`),语义与逐步
    /// [`commit_version`] 的可见性结果等价(`link_version` 仍维护 key/text 索引)。
    pub(crate) fn commit_recovered(
        &mut self,
        rowid: RowId,
        slot_data: SlotData,
        previous_exists: bool,
    ) -> Result<SlotId> {
        let slot = slot_id_for(self.slots.len())?;
        ensure_key_available(self, &slot_data)?;
        // 恢复输入按 `(rowid, seqno)` 排序:该 `rowid` 的首个版本无旧版本可遮蔽,
        // 跳过 `hide_latest` 的一次哈希查找(大批量恢复的每行常数项)。
        if previous_exists {
            self.hide_latest(rowid);
        }
        let arc = Arc::new(slot_data);
        Arc::make_mut(&mut self.slots).push(Arc::clone(&arc));
        self.slot_segment.push(None);
        self.link_version(rowid, slot);
        Ok(slot)
    }

    /// 把新提交的记录增量加入检索加速结构(倒排 / zone map / key bloom)。
    ///
    /// 墓碑不参与(zone 只统计实际字段值);被遮蔽的旧版本保留在索引中,
    /// 由查询期按视图可见性过滤,`as_of` 历史视图因此仍可检索旧版本文本。
    fn index_observe(&mut self, slot: SlotId, slot_data: &SlotData) {
        if let Some(text) = &slot_data.text {
            Arc::make_mut(&mut self.inv).insert_text(
                slot,
                slot_data.ns_id,
                text,
                self.stopwords_enabled,
            );
        }
        Arc::make_mut(&mut self.zones).observe(slot.get() as usize, slot_data);
        if let Some(key) = &slot_data.key {
            Arc::make_mut(&mut self.key_bloom).insert(key.as_str());
        }
    }

    /// 从槽位全量重建三类加速结构(无磁盘索引或映射不可用时使用)。
    ///
    /// 分词开关与字段上限取自本状态(建库/打开时由配置注入)。
    pub(crate) fn rebuild_indexes(&mut self) {
        self.inv = Arc::new(InvertedIndex::default());
        self.zones = Arc::new(ZoneIndex::new(self.index_fields_max));
        self.key_bloom = Arc::new(BloomSet::new(BLOOM_INITIAL_CAPACITY, self.bloom_fpp));
        for index in 0..self.slots.len() {
            let slot_data = Arc::clone(&self.slots[index]);
            if slot_data.deleted {
                continue;
            }
            // 槽位下标 ≤ u32::MAX(FC-MEM-INV-004),转换可证明不会失败。
            let slot =
                SlotId::new(u32::try_from(index).expect("槽位下标必可转入 u32(FC-MEM-INV-004)"));
            self.index_observe(slot, &slot_data);
        }
    }

    /// 装载磁盘倒排与 bloom,并从槽位重建 zone map。
    ///
    /// zone map 无法直接复用段内块统计:恢复按 `(rowid, seqno)` 重排槽位后,
    /// 段内块与全局块不再对应;重建结果与磁盘内容等价(roundtrip 测试保证)。
    pub(crate) fn load_disk_indexes(&mut self, inv: InvertedIndex, bloom: BloomSet) {
        self.inv = Arc::new(inv);
        self.key_bloom = Arc::new(bloom);
        self.zones = Arc::new(ZoneIndex::new(self.index_fields_max));
        for index in 0..self.slots.len() {
            if self.slots[index].deleted {
                continue;
            }
            Arc::make_mut(&mut self.zones).observe(index, &self.slots[index]);
        }
    }

    /// 装载多段合并倒排并按需重建 bloom / zone map(多段恢复路径)。
    ///
    /// `bloom = None` 时从全部槽位重建(各段 bloom 参数不一致时无法按位或合并);
    /// zone map 逐段块偏移与全局块不再对应,统一从槽位重建。
    pub(crate) fn load_merged_indexes(&mut self, inv: InvertedIndex, bloom: Option<BloomSet>) {
        self.inv = Arc::new(inv);
        self.zones = Arc::new(ZoneIndex::new(self.index_fields_max));
        self.key_bloom = Arc::new(BloomSet::new(BLOOM_INITIAL_CAPACITY, self.bloom_fpp));
        for index in 0..self.slots.len() {
            let slot_data = Arc::clone(&self.slots[index]);
            if slot_data.deleted {
                continue;
            }
            Arc::make_mut(&mut self.zones).observe(index, &slot_data);
            if let Some(key) = &slot_data.key {
                Arc::make_mut(&mut self.key_bloom).insert(key.as_str());
            }
        }
        if let Some(bloom) = bloom {
            self.key_bloom = Arc::new(bloom);
        }
    }

    /// 安装刚提交的段:登记槽位归属与段索引(供查询多图归并)。
    ///
    /// `slot_indices` 为该段包含的全局槽位(升序);`index = None` 表示该段无
    /// `hidx`(或未配置索引工厂),其槽位由查询期暴力覆盖。
    /// `quant`/`recall_est` 为该段实际生效的量化格式与建段抽样召回估计。
    pub(crate) fn install_segment(
        &mut self,
        segment_id: u32,
        slot_indices: &[usize],
        index: Option<Arc<dyn VectorIndex>>,
        quant: VectorFormat,
        recall_est: Option<f32>,
    ) {
        for &idx in slot_indices {
            if let Some(entry) = self.slot_segment.get_mut(idx) {
                *entry = Some(segment_id);
            }
        }
        if let Some(index) = index {
            let slots: Vec<SlotId> = slot_indices
                .iter()
                .map(|&idx| {
                    // 槽位下标 ≤ u32::MAX(FC-MEM-INV-004),转换可证明不会失败。
                    SlotId::new(u32::try_from(idx).expect("槽位下标必可转入 u32(FC-MEM-INV-004)"))
                })
                .collect();
            Arc::make_mut(&mut self.indexes).push(SegmentIndex::new(
                segment_id, index, slots, quant, recall_est,
            ));
        }
    }

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

    /// 清空自上次 flush 以来关系变更标记(全量重写关系表后调用)。
    pub(crate) fn clear_edge_dirty(&mut self) {
        self.edge_dirty = Arc::new(HashSet::new());
    }

    /// 记录本轮 compaction 回收的版本数。
    pub(crate) fn note_reclaimed(&mut self, count: usize) {
        self.reclaimed_versions = self.reclaimed_versions.saturating_add(count as u64);
    }

    /// 把已物理回收的槽位从版本链/索引中剪除并标死(段提交成功后调用)。
    ///
    /// 只剪引用、不重排槽位:被回收的物理槽位留在内存中由 `dead` 位图遮蔽,
    /// `as_of` 与当前读都不可见;内存重排(段句柄零拷贝)留待后续层。
    pub(crate) fn prune_reclaimed(&mut self, reclaimed: &[usize]) {
        for &index in reclaimed {
            // 槽位下标 ≤ u32::MAX(FC-MEM-INV-004),转换可证明不会失败。
            let slot =
                SlotId::new(u32::try_from(index).expect("槽位下标必可转入 u32(FC-MEM-INV-004)"));
            let rowid = self.slots[index].rowid;
            if self.detach_slot_from_versions(rowid, slot) {
                self.purge_row_indexes(rowid, index);
            }
            Arc::make_mut(&mut self.dead).set(index);
        }
    }

    /// 从版本链移除槽位;该 RowId 已无版本时移除 `latest` 并返回 `true`。
    fn detach_slot_from_versions(&mut self, rowid: RowId, slot: SlotId) -> bool {
        let empty = match self.versions.get_mut(&rowid) {
            Some(chain) => {
                let entries = Arc::make_mut(chain);
                entries.retain(|entry| *entry != slot);
                entries.is_empty()
            }
            None => return false,
        };
        if empty {
            self.versions.remove(&rowid);
            return true;
        }
        false
    }

    /// 整链清空时移除 `latest` 与 key/text/访问统计。
    fn purge_row_indexes(&mut self, rowid: RowId, index: usize) {
        self.latest.remove(&rowid);
        // 访问统计与待落盘增量一并清理,避免幽灵条目在后续 delta 中复活。
        self.access.remove(&rowid);
        Arc::make_mut(&mut self.access_dirty).remove(&rowid);
        let slot_data = &self.slots[index];
        if let Some(key) = &slot_data.key {
            let key = (slot_data.ns_id, key.clone());
            if self.key_index.get(&key) == Some(&rowid) {
                self.key_index.remove(&key);
            }
        }
        if let Some(hash) = slot_data.text_hash {
            let key = (slot_data.ns_id, hash);
            if self.text_index.get(&key) == Some(&rowid) {
                self.text_index.remove(&key);
            }
        }
    }

    /// 给 `rowid` 追加一个墓碑版本;若已无活版本则返回 `false`。
    pub(crate) fn tombstone(&mut self, rowid: RowId, tx_ms: i64, seqno: SeqNo) -> Result<bool> {
        let Some(latest) = self.latest.get(&rowid).copied() else {
            return Ok(false);
        };
        let base = Arc::clone(&self.slots[latest.get() as usize]);
        if base.deleted {
            return Ok(false);
        }
        let mut tomb = (*base).clone();
        tomb.seqno = seqno;
        tomb.tx_ms = tx_ms;
        tomb.deleted = true;
        self.commit_version(rowid, tomb)?;
        Ok(true)
    }

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
        }
    }
}

/// 校验新版本的 key 未被另一**可见**记录占用:占用者的最新版本已墓碑或逻辑
/// 过期时视为不存在(与读路径/FC-MEM-POST-001 同口径),否则返回
/// [`MnemeError::DuplicateKey`],绝不静默覆盖他人 `key_index`(FC-MEM-POST-007)。
fn ensure_key_available(ws: &WriterState, slot_data: &SlotData) -> Result<()> {
    let Some(key) = &slot_data.key else {
        return Ok(());
    };
    let Some(owner) = ws.key_index.get(&(slot_data.ns_id, key.clone())).copied() else {
        return Ok(());
    };
    if owner == slot_data.rowid {
        return Ok(());
    }
    let owner_visible = ws.latest.get(&owner).is_some_and(|slot| {
        let data = &ws.slots[slot.get() as usize];
        data.is_live(slot_data.tx_ms)
    });
    if owner_visible {
        return Err(MnemeError::DuplicateKey(key.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FC-MEM-INV-004
    #[test]
    fn slot_id_for_rejects_overflow() {
        assert_eq!(slot_id_for(0).expect("0 合法").get(), 0);
        assert_eq!(
            slot_id_for(u32::MAX as usize).expect("上界合法").get(),
            u32::MAX
        );
        let overflow = u32::MAX as usize + 1;
        assert!(matches!(
            slot_id_for(overflow),
            Err(MnemeError::LimitExceeded {
                field: "slots",
                limit,
                got,
            }) if limit == u32::MAX as usize && got == overflow
        ));
    }

    /// FC-PERSIST-INV-020 / FC-PERSIST-ERR-012:`RowId`/`NsId`/`SeqNo`
    /// 空间耗尽时返回 `IdExhausted`,绝不回绕复用。
    #[test]
    fn id_allocators_reject_exhaustion() {
        let mut state = WriterState::new();
        state.next_rowid = u64::MAX;
        assert!(matches!(
            state.alloc_rowid(),
            Err(MnemeError::IdExhausted { kind: "rowid" })
        ));
        state.seqno = SeqNo::new(u64::MAX);
        assert!(matches!(
            state.alloc_seqno(),
            Err(MnemeError::IdExhausted { kind: "seqno" })
        ));
        state.next_ns_id = u32::MAX;
        assert!(matches!(
            state.register_ns("耗尽"),
            Err(MnemeError::IdExhausted { kind: "ns_id" })
        ));
    }
}

#[cfg(test)]
mod relation_kind_tests {
    use std::sync::Arc;

    use super::*;

    /// FC-MODEL-POST-008:编号空间耗尽 → `TooLarge`;恢复登记冲突/越界 → `Corrupted`。
    #[test]
    fn relation_kind_registry_rejects_exhaustion() {
        let mut state = WriterState::new();
        state.next_rel_kind = u16::MAX;
        assert!(matches!(
            state.register_relation_kind("耗尽"),
            Err(MnemeError::TooLarge {
                field: "rel_kind",
                ..
            })
        ));

        let mut state = WriterState::new();
        state
            .register_recovered_rel_kind(16, Arc::from("mentions"))
            .expect("首次登记");
        assert_eq!(state.next_rel_kind, 17, "水位必须推进");
        assert!(matches!(
            state.register_recovered_rel_kind(17, Arc::from("mentions")),
            Err(MnemeError::Corrupted { .. })
        ));
        assert!(matches!(
            state.register_recovered_rel_kind(16, Arc::from("other")),
            Err(MnemeError::Corrupted { .. })
        ));
        assert!(matches!(
            state.register_recovered_rel_kind(3, Arc::from("builtin")),
            Err(MnemeError::Corrupted { .. })
        ));
        assert!(matches!(
            state.register_recovered_rel_kind(18, Arc::from("")),
            Err(MnemeError::Corrupted { .. })
        ));
    }
}
