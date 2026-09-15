//! MANIFEST 编解码(设计 04 §2.4、§6)。
//!
//! MANIFEST 是**命名空间路径与库级维度/度量的唯一事实来源**,也是活跃段集合与
//! ID 水位的持久载体;更新走 "write-once 版本文件 + `current` 指针"(§6)。
//!
//! ```text
//! 0   magic "MNF1" | 4 u16 ver | 6 u16 header_len | 8 u32 header_crc32
//! 12  u32 dimension | 16 u8 metric | 17 u8 stopwords | 18 reserved[4] | 22 u16 next_rel_kind
//! 24  u64 manifest_version | 32 u64 watermark_seqno | 40 u64 next_rowid
//! 48  u32 next_segment_id | 52 u32 next_ns_id | 56 u32 active_count
//! 60  u32 ns_count | 64 u32 rel_kind_count | 68..72 pad
//! 变长区: NsEntry×ns_count → RelKindEntry×rel_kind_count → SegmentEntry×active_count
//! 尾部:   u32 payload_crc32(覆盖变长区)
//! ```
//! `header_crc32` 覆盖头部除自身(偏移 8..12)外的全部字节。
//!
//! 子模块:条目类型与 ID 水位见 [`mod@model`],定长头部见 [`mod@header`],
//! 编码见 [`mod@encode`],解码与校验见 [`mod@parse`]。

mod encode;
mod header;
mod model;
mod parse;

pub(crate) use encode::encode;
// `MAGIC`/`HEADER_LEN` 仅模块内部使用,但拆分不改变原 `pub(crate)` 路径
// `crate::persist::manifest::{MAGIC, HEADER_LEN}` 的可达性。
#[allow(unused_imports)]
pub(crate) use header::{HEADER_LEN, MAGIC};
pub(crate) use model::{
    Manifest, NsEntry, RelKindEntry, SegmentEntry, next_manifest_version, next_segment_id,
};
pub(crate) use parse::parse;

#[cfg(test)]
mod tests;
