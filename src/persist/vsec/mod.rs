//! 向量段(`vsec`)编解码(设计 04 §2.1、08 §5)。
//!
//! 文件 = 64 字节定长头 + 数据区(向量区 / norm 区 / 量化副本区 / 删除位图)
//! + 尾部 payload CRC。整数一律小端;定长字段按自然对齐摆放,便于 mmap 后零拷贝读取。
//!
//! ```text
//! 0   magic "VSC1" | 4 u16 ver | 6 u16 header_len | 8 u32 dimension
//! 12  u8 metric | 13 u8 quant | 14 u8 norm_col | 15 u8 reserved
//! 16  u64 row_count | 24 i64 created_unix_ms | 32 u32 header_crc32 | 36..64 pad
//! 数据区: vec(每行补齐到 32B) → norm(可选) → qvec(quant != F32 时)
//!         → del_bitmap(每 1024 行 128B)
//! 尾部:   u32 payload_crc32(覆盖整个数据区)
//! ```
//!
//! 删除位图的 `1` 表示该物理槽位**当前不可见**(被遮蔽/删除);每个 1024 行块
//! 用 16 个 `u64`(128 B)承载,末块未用槽恒置 `1`。
//!
//! 量化副本区(`quant != F32`,L6)布局(FC-QUANT-POST-002):
//! * i8: 段级逐维 `(v_min, v_max)` 交错表(`2d` 个 f32,LE) + `row_count × d` 字节码;
//! * f16: `row_count × 2d` 字节码(IEEE 754 half,LE)。
//!
//! f32 原向量始终保留在 `vec` 区供精排;未知 `quant` 编码与畸形参数表在解析期
//! 返回 `Corrupted`,绝不部分解析(FC-QUANT-ERR-003)。

mod encode;
mod header;
mod view;

#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::core::error::MnemeError;
#[cfg(test)]
use crate::core::metric::Metric;
#[cfg(test)]
use crate::core::options::VectorFormat;
#[cfg(test)]
use crate::persist::crc32;

pub(crate) use encode::encode;
pub(crate) use header::{VsecInput, metric_from_u8, metric_to_u8};
// 旧路径 `crate::persist::vsec::X` 要保持可达;crate 里只在部分 feature 组合下用,放行警告。
#[allow(unused_imports)]
pub(crate) use header::{HEADER_LEN, MAGIC, VsecHeader, quant_from_u8, quant_to_u8};
pub(crate) use view::{VsecView, parse};
