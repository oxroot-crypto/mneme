//! 内存表结构、写状态与不可变读视图。
//!
//! 写路径由 [`Table::writer`] 串行;每次写入完成后由 [`Table::publish`] 把
//! [`WriterState`] 的 `Arc` 句柄快照成一份 [`ReaderView`] 并原子发布,
//! 读者克隆该 `Arc` 后即可无锁扫描——这是设计 03 §3/§7 的 L1 落地。
//!
//! 物理版本以 [`SlotData`] 表示,下标即 `SlotId`、**只增不减**;被遮蔽/删除的
//! 版本以 `dead` 位图标记,`as_of` 仍可经版本链读取历史。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::types::{Key, NsId, RowId, SeqNo, SlotId};
use crate::memory::config::Config;
use crate::memory::relation::Edge;

/// 位图每个字(`u64`)的位数。
const BITS_PER_WORD: usize = u64::BITS as usize;

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

/// 单条记录的访问统计(内存累积;L5 起落盘)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccessStat {
    /// 最近一次访问时刻(Unix 毫秒)。
    pub last_access_ms: i64,
    /// 累计访问次数。
    pub access_count: u32,
}

/// 可增长的位图,用于标记不可见物理版本。
#[derive(Debug, Clone, Default)]
pub(crate) struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    /// 置位第 `idx` 位,必要时扩容。
    pub(crate) fn set(&mut self, idx: usize) {
        let word = idx / BITS_PER_WORD;
        if word >= self.words.len() {
            self.words.resize(word + 1, 0);
        }
        self.words[word] |= 1_u64 << (idx % BITS_PER_WORD);
    }

    /// 读取第 `idx` 位。
    pub(crate) fn get(&self, idx: usize) -> bool {
        self.words
            .get(idx / BITS_PER_WORD)
            .is_some_and(|word| (word >> (idx % BITS_PER_WORD)) & 1 == 1)
    }

    /// 清除第 `idx` 位。
    pub(crate) fn clear(&mut self, idx: usize) {
        if let Some(word) = self.words.get_mut(idx / BITS_PER_WORD) {
            *word &= !(1_u64 << (idx % BITS_PER_WORD));
        }
    }
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
    pub(crate) closed: bool,
}

impl WriterState {
    fn new() -> Self {
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
        Arc::make_mut(&mut self.ns_by_path).insert(path, id);
        id
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
    /// 容量校验在遮蔽旧版本**之前**完成:槽位溢出(`u32::MAX`)时返回结构化错误且
    /// 不改动旧版本,消除「已遮蔽但无新版本」的半写(FC-MEM-PRE-002 零部分写入)。
    pub(crate) fn commit_version(&mut self, rowid: RowId, slot_data: SlotData) -> Result<SlotId> {
        let slot = slot_id_for(self.slots.len())?;
        self.hide_latest(rowid);
        Arc::make_mut(&mut self.slots).push(Arc::new(slot_data));
        self.link_version(rowid, slot);
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

/// 不可变读视图:读者克隆后无锁扫描。
pub(crate) struct ReaderView {
    pub(crate) slots: Arc<Vec<Arc<SlotData>>>,
    pub(crate) dead: Arc<BitSet>,
    pub(crate) key_index: Arc<HashMap<(NsId, Key), RowId>>,
    pub(crate) versions: Arc<HashMap<RowId, Vec<SlotId>>>,
    pub(crate) latest: Arc<HashMap<RowId, SlotId>>,
    pub(crate) out_edges: Arc<HashMap<RowId, Vec<Edge>>>,
    pub(crate) in_edges: Arc<HashMap<RowId, Vec<Edge>>>,
    pub(crate) access: Arc<HashMap<RowId, AccessStat>>,
    pub(crate) ns_registry: Arc<HashMap<NsId, Arc<str>>>,
    pub(crate) seqno: SeqNo,
    /// 库是否已关闭(关闭后读写返回 `Closed`)。
    pub(crate) closed: bool,
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
}

/// 内存表:写状态 + 已发布读视图 + 配置。
pub(crate) struct Table {
    pub(crate) writer: Mutex<WriterState>,
    pub(crate) reader: RwLock<Arc<ReaderView>>,
    pub(crate) config: Arc<Config>,
}

impl Table {
    /// 以给定配置新建空表。
    pub(crate) fn new(config: Arc<Config>) -> Self {
        let writer = WriterState::new();
        let view = Arc::new(writer.snapshot());
        Self {
            writer: Mutex::new(writer),
            reader: RwLock::new(view),
            config,
        }
    }

    /// 获取写锁;锁中毒时恢复内部数据继续工作(不 panic)。
    pub(crate) fn write(&self) -> MutexGuard<'_, WriterState> {
        self.writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 获取读锁。
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, Arc<ReaderView>> {
        self.reader
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 克隆当前读视图。
    pub(crate) fn view(&self) -> Arc<ReaderView> {
        Arc::clone(&self.read())
    }

    /// 把写状态发布为新的读视图(调用方须持有写锁)。
    pub(crate) fn publish(&self, ws: &WriterState) {
        let view = Arc::new(ws.snapshot());
        let mut guard: RwLockWriteGuard<'_, Arc<ReaderView>> = self
            .reader
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = view;
    }

    /// 在写事务中执行 `f`:进入前快照写状态,`f` 返回 `Err` 时回滚到快照、
    /// 不发布;成功时发布读视图。
    ///
    /// 所有容器字段均为 `Arc`,`clone` 仅复制句柄,故快照/回滚廉价。此机制保证
    /// 任何失败的写操作对读者零可见、不留半写(FC-MEM-POST-002 泛化),并让
    /// 失败写入不残留命名空间登记等副作用。
    pub(crate) fn write_tx<T>(&self, f: impl FnOnce(&mut WriterState) -> Result<T>) -> Result<T> {
        let mut ws = self.write();
        let snapshot = ws.clone();
        match f(&mut ws) {
            Ok(value) => {
                self.publish(&ws);
                Ok(value)
            }
            Err(error) => {
                *ws = snapshot;
                Err(error)
            }
        }
    }
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
