//! 跨段覆盖条目 [`DeltaEntry`] 同其公共前缀与排序键辅助方法。

use crate::core::meta::Meta;

use super::consts::{KIND_ACCESS, KIND_RELATE, KIND_UNRELATE};

/// 一条跨段覆盖条目(仅访问统计与关系边;删除/更新由版本行承载)。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DeltaEntry {
    /// 访问统计增量:恢复时 `access_count += access_delta`, `last_access_ms = at_ms`。
    Access {
        /// 全局写序号(排序/审计用)。
        seqno: u64,
        /// 事务时间(Unix 毫秒)。
        tx_ms: i64,
        /// 所属命名空间(审计用;应用不依赖)。
        ns_id: u32,
        /// 目标记录。
        rowid: u64,
        /// 最近访问时刻(Unix 毫秒)。
        last_access_ms: i64,
        /// 自上次物化以来的访问次数增量。
        access_delta: u32,
        /// 重要性提升增量(显式 `touch(boost)` 走版本行,本字段恒 0;格式保留)。
        importance_delta: f32,
    },
    /// 新增/更新关系边。
    Relate {
        /// 全局写序号。
        seqno: u64,
        /// 事务时间(Unix 毫秒)。
        tx_ms: i64,
        /// 起点所在命名空间(审计用)。
        ns_id: u32,
        /// 起点。
        from: u64,
        /// 终点。
        to: u64,
        /// 关系类型编号。
        kind: u16,
        /// 边权。
        weight: f32,
        /// 边元数据。
        meta: Meta,
    },
    /// 删除关系边。
    Unrelate {
        /// 全局写序号。
        seqno: u64,
        /// 事务时间(Unix 毫秒)。
        tx_ms: i64,
        /// 起点所在命名空间(审计用)。
        ns_id: u32,
        /// 起点。
        from: u64,
        /// 终点。
        to: u64,
        /// 关系类型编号。
        kind: u16,
    },
}

impl DeltaEntry {
    /// 公共前缀字段。
    pub(super) fn common(&self) -> (u8, u64, i64, u32) {
        match self {
            DeltaEntry::Access {
                seqno,
                tx_ms,
                ns_id,
                ..
            } => (KIND_ACCESS, *seqno, *tx_ms, *ns_id),
            DeltaEntry::Relate {
                seqno,
                tx_ms,
                ns_id,
                ..
            } => (KIND_RELATE, *seqno, *tx_ms, *ns_id),
            DeltaEntry::Unrelate {
                seqno,
                tx_ms,
                ns_id,
                ..
            } => (KIND_UNRELATE, *seqno, *tx_ms, *ns_id),
        }
    }

    /// 排序目标(设计 04 §2.2a:按 `(target, seqno, kind)` 排序)。
    pub(super) fn target(&self) -> u64 {
        match self {
            DeltaEntry::Access { rowid, .. } => *rowid,
            DeltaEntry::Relate { from, .. } | DeltaEntry::Unrelate { from, .. } => *from,
        }
    }

    /// 条目种类编号(排序次级键)。
    pub(super) fn kind_rank(&self) -> u8 {
        self.common().0
    }
}
