//! msec 轻量索引区编解码(`msec/index/`,设计 04 §2.2、§5)。
//!
//! 本文件承载三类区;倒排区编解码见 [`inverted`](super::inverted)。
//!
//! ```text
//! field_dict: [u32 count]
//!             每字段: [u16 field_id][u8 kind(0=Num 1=Ts 2=Str)][u32 name_len][name]
//! zmap:       [u32 field_count][u64 block_count]
//!             每数值字段: [u32 field_id][block × (f64 min, f64 max, u8 flags)]
//!                       flags: bit0=has_value bit1=has_null;±∞ 表示"区间未知"
//!             [i64 × block_count]每块 min(expires_at),无 TTL 记 +∞
//!                       —— 块级 TTL 剪枝:min > now ⇒ 整块记录全未过期,免逐行判定
//! bloom:      [u32 count] 每条: [u16 field_id][u32 bit_len][u32 k][u64 × bit_len/64]
//! ```
//!
//! 所有区在 msec 同一 payload CRC 保护下,与记录体同生同灭。

mod bloom;
mod common;
mod field_dict;
mod zmap;

// 拆分不改外部可达性:旧路径 `crate::persist::msec::index::{...}` 照旧。
pub(crate) use bloom::{decode_bloom, encode_bloom};
pub(super) use common::{corrupted, read_utf8};
pub(crate) use field_dict::{FieldDef, FieldKind, decode_field_dict, encode_field_dict};
pub(crate) use zmap::{decode_ttl_map, encode_ttl_map, encode_zmap, validate_zmap};

#[cfg(test)]
mod tests;

// 测试经 `use super::*` 取箇滴名字(拆分前由本模块顶层 import 提供)。
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use crate::core::error::MnemeError;
#[cfg(test)]
use crate::memory::analysis::{BloomSet, ZoneIndex};
#[cfg(test)]
use crate::persist::{put_bytes_u32, put_u16, put_u32, put_u64};
