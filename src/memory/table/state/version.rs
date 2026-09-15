//! 版本链提交、遮蔽、墓碑与回收剪除。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::types::{Key, NsId, RowId, SeqNo, SlotId};
use crate::memory::table::write_op::WriteOp;

use super::{SlotData, WriterState, slot_id_for};

impl WriterState {
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
}

impl WriterState {
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
