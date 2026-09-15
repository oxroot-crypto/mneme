//! msec 倒排区编解码(`msec/inverted/`,设计 04 §2.2、§5)。
//!
//! 区布局:
//!
//! ```text
//! [u32 term_count]
//! 每词: [u32 ns_id][u32 term_len][term][u32 df]
//!       [u64 postings_offset(相对 postings 区起点)][u32 postings_len]
//! [u64 postings_total_len]
//! postings 区: 每词 [varint slot_delta][varint tf] × df(段内 SlotId 升序)
//! doc 区: [u32 doc_count] + 每文档 [u32 ns_id][u32 slot][u32 doc_len]
//! ```
//!
//! `postings_total_len` 显式给出 postings 区长度,doc 区起点不再靠"各词条
//! 声明区间的最大值"推断,畸形文件无法把 postings 读取引向 doc 区;
//! 所有长度/偏移均以 `checked` 运算校验,畸形文件返回 `Corrupted`,
//! 绝不 panic 或回绕。

mod decode;
mod encode;
mod postings;

// 拆分唔改外部可达性:旧路径 `crate::persist::msec::inverted::{...}` 照旧。
pub(crate) use decode::decode_inverted;
pub(crate) use encode::encode_inverted;

#[cfg(test)]
mod tests;

// 测试经 `use super::*` 取箇滴名字(拆分前由本模块顶层 import 提供)。
#[cfg(test)]
use crate::core::error::MnemeError;
#[cfg(test)]
use crate::core::types::{NsId, SlotId};
#[cfg(test)]
use crate::core::varint;
#[cfg(test)]
use crate::memory::analysis::InvertedIndex;
#[cfg(test)]
use crate::persist::{put_bytes_u32, put_u32, put_u64};
#[cfg(test)]
use postings::decode_postings;
