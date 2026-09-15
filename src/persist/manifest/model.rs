//! MANIFEST 条目类型与 ID 水位推进。
//!
//! 命名空间/关系类型/活跃段三类条目是 MANIFEST 的载荷;段号与 MANIFEST
//! 版本号水位只增不回绕(FC-PERSIST-ERR-012)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;

/// 命名空间注册项(`path ↔ NsId`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NsEntry {
    /// 命名空间编号。
    pub(crate) ns_id: u32,
    /// 命名空间路径(UTF-8)。
    pub(crate) path: Arc<str>,
}

/// 关系类型注册项(`kind ↔ name`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelKindEntry {
    /// 关系类型编号。
    pub(crate) kind: u16,
    /// 类型名(UTF-8)。
    pub(crate) name: Arc<str>,
}

/// 活跃段条目。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SegmentEntry {
    /// 段编号。
    pub(crate) segment_id: u32,
    /// 段文件格式版本(审计用;打开时以段文件自身头部的版本为权威,不读本字段)。
    pub(crate) format_version: u16,
    /// 行数。
    pub(crate) row_count: u64,
    /// 最小写序号。
    pub(crate) min_seqno: u64,
    /// 最大写序号。
    pub(crate) max_seqno: u64,
    /// 创建时刻(Unix 毫秒)。
    pub(crate) created_ms: i64,
    /// vsec 文件 CRC。
    pub(crate) vsec_crc: u32,
    /// msec 文件 CRC。
    pub(crate) msec_crc: u32,
    /// hidx 文件 CRC(无 hidx 时为 0)。
    pub(crate) hidx_crc: u32,
    /// HNSW 入口槽位(L3 起;L2 为 0)。
    pub(crate) entry_slot: u32,
    /// HNSW 入口层级(L3 起;L2 为 0)。
    pub(crate) entry_level: u8,
}

/// MANIFEST 全部字段。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Manifest {
    /// 建库维度(建库即锁定)。
    pub(crate) dimension: u32,
    /// 距离度量(建库即锁定)。
    pub(crate) metric: Metric,
    /// 文本分词是否启用停用词(建库即锁定;查询与索引必须同口径)。
    pub(crate) stopwords: bool,
    /// 自定义关系类型编号分配水位(永不复用)。
    pub(crate) next_rel_kind: u16,
    /// MANIFEST 版本号。
    pub(crate) manifest_version: u64,
    /// 已物化水位:回放只处理 `seqno > watermark_seqno` 的帧。
    pub(crate) watermark_seqno: u64,
    /// 全局 RowId 分配水位(永不复用)。
    pub(crate) next_rowid: u64,
    /// 段编号分配水位。
    pub(crate) next_segment_id: u32,
    /// 命名空间编号分配水位(永不复用)。
    pub(crate) next_ns_id: u32,
    /// 命名空间注册表。
    pub(crate) namespaces: Vec<NsEntry>,
    /// 关系类型注册表。
    pub(crate) rel_kinds: Vec<RelKindEntry>,
    /// 活跃段集合。
    pub(crate) segments: Vec<SegmentEntry>,
}

/// 分配下一个段号;`u32::MAX` 已无法再分配 → `IdExhausted`(FC-PERSIST-ERR-012)。
///
/// # Errors
/// `current == u32::MAX` 时返回 [`MnemeError::IdExhausted`]。
pub(crate) fn next_segment_id(current: u32) -> Result<u32> {
    current
        .checked_add(1)
        .ok_or(MnemeError::IdExhausted { kind: "segment_id" })
}

/// 推进 MANIFEST 版本号;`u64::MAX` 已到表示上限 → `IdExhausted`。
///
/// # Errors
/// `current == u64::MAX` 时返回 [`MnemeError::IdExhausted`]。
pub(crate) fn next_manifest_version(current: u64) -> Result<u64> {
    current.checked_add(1).ok_or(MnemeError::IdExhausted {
        kind: "manifest_version",
    })
}
