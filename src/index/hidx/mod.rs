//! `hidx`(HNSW 图段)字节布局(设计 05 §10)。
//!
//! ```text
//! 0   magic "HID1" | 4 u16 ver | 6 u16 header_len | 8 u16 m | 10 u16 m0
//! 12  u16 ef_construction | 14 u8 entry_level | 15 u8 reserved | 16 f32 ml
//! 20  u32 count | 24 u32 entry_slot | 28 u32 node_table_len | 32 u32 adj_len
//! 36  u32 header_crc32(覆盖 [0,36)) | 40..64 padding
//! node_table(64 起,count × 5 B): (u8 level, u32 adj_off)   # adj_off 相对 adj_blob 起点
//! adj_blob: 逐节点逐层 [u16 degree][degree × u32 邻居节点 id]
//! 尾部:     u32 payload_crc32(覆盖 node_table || adj_blob)
//! ```
//!
//! 整数小端。节点 id 为段内槽位下标;载入后按恢复重排映射到全局槽位。

mod encode;
mod header;
mod layout;
mod read;

// `MAGIC` 仅 feature `fuzzing` 的 `src/fuzzing.rs` 使用,但旧路径可达性不随 feature 改变。
#[allow(unused_imports)]
pub(crate) use encode::{GraphParams, MAGIC, encode};
// `HEADER_LEN`/`NODE_TABLE_ENTRY` 仅模块内部与测试使用,但拆分不改变原 `pub(crate)`
// 路径 `crate::index::hidx::{HEADER_LEN, NODE_TABLE_ENTRY}` 的可达性。
#[allow(unused_imports)]
pub(crate) use encode::{HEADER_LEN, NODE_TABLE_ENTRY};
pub(crate) use read::{open, verify};

// `Decoded` 仅测试路径使用,但拆分不改变原 `pub(crate)` 路径的可达性。
#[allow(unused_imports)]
#[cfg(test)]
pub(crate) use read::{Decoded, decode};

#[cfg(test)]
mod tests;
