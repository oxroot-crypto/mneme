//! delta 区格式常量同条目 kind 编号。

/// delta 区魔数。
pub(crate) const MAGIC: [u8; 4] = *b"DLT1";
/// delta 区格式版本。
pub(crate) const FORMAT_VERSION: u16 = 0x0001;
/// 区头长度:`magic(4) + ver(2) + count(2) + crc(4)`。
pub(super) const HEADER_LEN: usize = 12;
/// 单条 delta 的公共前缀:`kind(1) + seqno(8) + tx_ms(8) + ns_id(4)`。
pub(super) const COMMON_BYTES: usize = 21;

pub(super) const KIND_ACCESS: u8 = 4;
pub(super) const KIND_RELATE: u8 = 5;
pub(super) const KIND_UNRELATE: u8 = 6;
