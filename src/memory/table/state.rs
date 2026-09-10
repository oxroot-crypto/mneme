//! 物理槽位与写状态(`table/state.rs`)。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::options::RelationKind;
use crate::core::types::{Key, NsId, RowId, SeqNo, SlotId};
use crate::memory::bitset::BitSet;
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
    pub(crate) vector: Arc<[f32]>,
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
    pub(crate) key_index: Arc<HashMap<(NsId, Key), RowId>>,
    pub(crate) text_index: Arc<HashMap<(NsId, u64), RowId>>,
    pub(crate) versions: Arc<HashMap<RowId, Vec<SlotId>>>,
    pub(crate) latest: Arc<HashMap<RowId, SlotId>>,
    pub(crate) out_edges: Arc<HashMap<RowId, Vec<Edge>>>,
    pub(crate) in_edges: Arc<HashMap<RowId, Vec<Edge>>>,
    pub(crate) access: Arc<HashMap<RowId, AccessStat>>,
    pub(crate) seqno: SeqNo,
    pub(crate) next_rowid: u64,
    pub(crate) next_ns_id: u32,
    pub(crate) ns_registry: Arc<HashMap<NsId, Arc<str>>>,
    pub(crate) ns_by_path: Arc<HashMap<Arc<str>, NsId>>,
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
            key_index: Arc::new(HashMap::new()),
            text_index: Arc::new(HashMap::new()),
            versions: Arc::new(HashMap::new()),
            latest: Arc::new(HashMap::new()),
            out_edges: Arc::new(HashMap::new()),
            in_edges: Arc::new(HashMap::new()),
            access: Arc::new(HashMap::new()),
            seqno: SeqNo::new(0),
            next_rowid: 0,
            next_ns_id: 1,
            ns_registry: Arc::new(HashMap::new()),
            ns_by_path: Arc::new(HashMap::new()),
            feedback_seen: Arc::new(HashSet::new()),
            pending: Vec::new(),
            closed: false,
        }
    }

    /// 分配下一个全局单调 `SeqNo`。
    pub(crate) fn alloc_seqno(&mut self) -> SeqNo {
        self.seqno = SeqNo::new(self.seqno.get() + 1);
        self.seqno
    }

    /// 分配下一个稳定 `RowId`。
    pub(crate) fn alloc_rowid(&mut self) -> RowId {
        let rowid = RowId::new(self.next_rowid);
        self.next_rowid += 1;
        rowid
    }

    /// 返回命名空间路径对应的 `NsId`(不存在则 `None`)。
    pub(crate) fn ns_id_of(&self, path: &str) -> Option<NsId> {
        self.ns_by_path.get(path).copied()
    }

    /// 注册命名空间路径(已存在则返回既有 `NsId`),分配单调 `NsId`。
    pub(crate) fn register_ns(&mut self, path: &str) -> NsId {
        if let Some(id) = self.ns_by_path.get(path) {
            return *id;
        }
        let id = NsId::new(self.next_ns_id);
        self.next_ns_id += 1;
        let path: Arc<str> = Arc::from(path);
        Arc::make_mut(&mut self.ns_registry).insert(id, path.clone());
        Arc::make_mut(&mut self.ns_by_path).insert(path.clone(), id);
        self.pending.push(WriteOp::NsRegister {
            ns_id: id.get(),
            path,
        });
        id
    }

    /// 建立/更新关系边并记录 WAL 操作。
    pub(crate) fn relate_edge(&mut self, edge: Edge) {
        crate::memory::relation::upsert_edge(Arc::make_mut(&mut self.out_edges), edge.clone());
        crate::memory::relation::upsert_edge(Arc::make_mut(&mut self.in_edges), edge.clone());
        let seqno = self.alloc_seqno();
        self.pending.push(WriteOp::Relate {
            from: edge.from,
            to: edge.to,
            kind: edge.kind.0,
            weight: edge.weight,
            meta: edge.metadata,
            seqno,
        });
    }

    /// 删除关系边并记录 WAL 操作;未命中返回 `false` 且不消耗序号。
    pub(crate) fn unrelate_edge(&mut self, from: RowId, to: RowId, kind: RelationKind) -> bool {
        let removed = crate::memory::relation::remove_edge(
            Arc::make_mut(&mut self.out_edges),
            from,
            to,
            kind,
        );
        crate::memory::relation::remove_edge(Arc::make_mut(&mut self.in_edges), to, from, kind);
        if removed {
            let seqno = self.alloc_seqno();
            self.pending.push(WriteOp::Unrelate {
                from,
                to,
                kind: kind.0,
                seqno,
            });
        }
        removed
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
                Arc::make_mut(&mut self.key_index).remove(&(ns_id, old_key));
            }
        }
        Arc::make_mut(&mut self.versions)
            .entry(rowid)
            .or_default()
            .push(slot);
        Arc::make_mut(&mut self.latest).insert(rowid, slot);
        let slot_data = Arc::clone(&self.slots[slot.get() as usize]);
        if let Some(key) = &slot_data.key {
            Arc::make_mut(&mut self.key_index).insert((slot_data.ns_id, key.clone()), rowid);
        }
        if let Some(hash) = slot_data.text_hash {
            Arc::make_mut(&mut self.text_index).insert((slot_data.ns_id, hash), rowid);
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
        self.link_version(rowid, slot);
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
            key_index: Arc::clone(&self.key_index),
            versions: Arc::clone(&self.versions),
            latest: Arc::clone(&self.latest),
            out_edges: Arc::clone(&self.out_edges),
            in_edges: Arc::clone(&self.in_edges),
            access: Arc::clone(&self.access),
            ns_registry: Arc::clone(&self.ns_registry),
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
}
