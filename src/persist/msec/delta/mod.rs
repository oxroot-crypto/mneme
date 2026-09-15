//! msec `delta` 区(跨段覆盖)编解码(设计 04 §2.2a)。
//!
//! 增量段只物化新版本槽位;对**已落盘旧段**的非版本化变更(访问统计、关系边)
//! 以 delta 条目承载,恢复时按段序回放。完整新版本(含 `touch` boost 产生的
//! 版本)仍走 `version_table`,不重复登记。
//!
//! ```text
//! delta: [magic "DLT1"][u16 ver][u16 count][u32 entry_crc32]
//!        条目 × count(按 (target, seqno, kind) 排序):
//!          [u8 kind][u64 seqno][i64 tx_ms][u32 ns_id]
//!          kind=4 Access  : [RowId u64][i64 last_access_ms][u32 access_delta][f32 importance_delta]
//!          kind=5 Relate  : [from u64][to u64][kind u16][f32 weight][meta len+bytes]
//!          kind=6 Unrelate: [from u64][to u64][kind u16]
//! ```
//!
//! kind 1–3(`DeleteKey`/`DeleteRow`/`UpdateRow`)由设计保留:本层无需以 delta
//! 表示删除/更新(它们总以版本行承载),故编解码一律拒绝为 [`MnemeError::Corrupted`]。
//! 空区表示本段无跨段变更,解码为空 `Vec`。
//!
//! # 子模块
//!
//! * `consts` —— 区头/条目公共常量同 kind 编号。
//! * `entry` —— 跨段覆盖条目 [`DeltaEntry`]。
//! * `codec` —— delta 区编解码。

mod codec;
mod consts;
mod entry;

#[cfg(test)]
mod tests;

// 仅为模块级 rustdoc 链接 [`MnemeError::Corrupted`] 可解析而引入,不参与代码路径。
#[allow(unused_imports)]
use crate::core::error::MnemeError;

// 拆分唔改外部可达性:旧路径 `crate::persist::msec::delta::{...}` 照旧。
pub(crate) use codec::{decode_delta, encode_delta};
pub(crate) use entry::DeltaEntry;
// 区头常量暂无库内消费者,保留 re-export 只为旧路径可达(旧模块曾是 `pub(crate)`)。
#[allow(unused_imports)]
pub(crate) use consts::{FORMAT_VERSION, MAGIC};

// 测试经 `use super::*` 取箇滴名字(拆分前由本模块顶层 import/定义提供)。
#[cfg(test)]
use crate::persist::crc32;
#[cfg(test)]
use consts::{COMMON_BYTES, HEADER_LEN};
