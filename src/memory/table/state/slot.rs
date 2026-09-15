//! 物理槽位、槽位下标映射与访问统计。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::types::{Key, NsId, RowId, SeqNo, SlotId};
use crate::memory::lazy::VectorStorage;

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
