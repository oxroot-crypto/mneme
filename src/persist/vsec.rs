//! 向量段(`vsec`)编解码(设计 04 §2.1)。
//!
//! 文件 = 64 字节定长头 + 数据区(向量区 / norm 区 / 删除位图)+ 尾部 payload CRC。
//! 整数一律小端;定长字段按自然对齐摆放,便于 mmap 后零拷贝读取。
//!
//! ```text
//! 0   magic "VSC1" | 4 u16 ver | 6 u16 header_len | 8 u32 dimension
//! 12  u8 metric | 13 u8 quant | 14 u8 norm_col | 15 u8 reserved
//! 16  u64 row_count | 24 i64 created_unix_ms | 32 u32 header_crc32 | 36..64 pad
//! 数据区: vec(每行补齐到 32B) → norm(可选) → del_bitmap(每 1024 行 128B)
//! 尾部:   u32 payload_crc32(覆盖整个数据区)
//! ```
//!
//! 删除位图的 `1` 表示该物理槽位**当前不可见**(被遮蔽/删除);每个 1024 行块
//! 用 16 个 `u64`(128 B)承载,末块未用槽恒置 `1`。量化副本(`quant != 0`)属 L6,
//! 本层解码时显式返回 `Unsupported`,绝不静默忽略。

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::persist::{Cursor, FORMAT_VERSION, align_up, check_version, crc32};

/// 向量段魔数。
pub(crate) const MAGIC: [u8; 4] = *b"VSC1";
/// 定长头部长度(字节)。
pub(crate) const HEADER_LEN: u16 = 64;
/// 每行向量的对齐粒度(字节)。
const ROW_ALIGN: usize = 32;
/// 删除位图分块行数。
const BITMAP_BLOCK_ROWS: usize = 1024;
/// 删除位图单块字节数(16 × u64)。
const BITMAP_BLOCK_BYTES: usize = 128;

/// 向量段头部字段。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VsecHeader {
    /// 建库维度。
    pub(crate) dimension: u32,
    /// 距离度量。
    pub(crate) metric: Metric,
    /// 量化副本格式(0 = F32;其余为 L6 量化副本)。
    pub(crate) quant: u8,
    /// 是否附带 norm 列。
    pub(crate) norm_col: bool,
    /// 行数。
    pub(crate) row_count: u64,
    /// 创建时刻(Unix 毫秒)。
    pub(crate) created_unix_ms: i64,
}

/// 向量段编码输入。
pub(crate) struct VsecInput<'a> {
    /// 维度。
    pub(crate) dimension: u32,
    /// 度量。
    pub(crate) metric: Metric,
    /// 创建时刻(Unix 毫秒)。
    pub(crate) created_unix_ms: i64,
    /// 每行向量;长度即 `row_count`。
    pub(crate) vectors: &'a [&'a [f32]],
    /// 每行范数平方;`norm_col` 恒为 `true`。
    pub(crate) norms: &'a [f32],
    /// 每行是否不可见(`true` = 删除位图置位)。
    pub(crate) dead: &'a [bool],
}

/// 行跨距:向量字节数向上对齐到 32B。
const fn row_stride(dimension: u32) -> usize {
    align_up(dimension as usize * std::mem::size_of::<f32>(), ROW_ALIGN)
}

/// 删除位图总字节数(每 1024 行一块、每块 128 B)。
const fn bitmap_bytes(row_count: u64) -> usize {
    row_count.div_ceil(BITMAP_BLOCK_ROWS as u64) as usize * BITMAP_BLOCK_BYTES
}

/// 度量 → 字节编码(0=Cosine 1=Dot 2=Euclidean)。
pub(crate) const fn metric_to_u8(metric: Metric) -> u8 {
    match metric {
        Metric::Cosine => 0,
        Metric::Dot => 1,
        Metric::Euclidean => 2,
    }
}

/// 字节编码 → 度量;越界返回 `Corrupted`。
pub(crate) fn metric_from_u8(value: u8) -> Result<Metric> {
    match value {
        0 => Ok(Metric::Cosine),
        1 => Ok(Metric::Dot),
        2 => Ok(Metric::Euclidean),
        _ => Err(MnemeError::Corrupted {
            segment: None,
            reason: format!("vsec: 未知度量编码 {value}"),
        }),
    }
}

/// 编码一个完整的向量段文件。
///
/// # Errors
/// 输入不一致(行数/维度不匹配、`quant` 非 F32)或维度非法时返回结构化错误。
pub(crate) fn encode(input: &VsecInput<'_>) -> Result<Vec<u8>> {
    let count = input.vectors.len();
    if input.norms.len() != count || input.dead.len() != count {
        return Err(MnemeError::Config {
            reason: "vsec 编码:vectors/norms/dead 长度不一致",
        });
    }
    for vector in input.vectors {
        if vector.len() != input.dimension as usize {
            return Err(MnemeError::DimensionMismatch {
                expected: input.dimension,
                got: vector.len(),
            });
        }
    }

    let header = encode_header(&VsecHeader {
        dimension: input.dimension,
        metric: input.metric,
        quant: 0,
        norm_col: true,
        row_count: count as u64,
        created_unix_ms: input.created_unix_ms,
    })?;

    let stride = row_stride(input.dimension);
    let mut data = Vec::with_capacity(count * stride + count * 4 + bitmap_bytes(count as u64));
    for vector in input.vectors {
        for value in *vector {
            data.extend_from_slice(&value.to_le_bytes());
        }
        data.resize(data.len() + (stride - vector.len() * 4), 0);
    }
    for norm in input.norms {
        data.extend_from_slice(&norm.to_le_bytes());
    }
    data.extend_from_slice(&encode_bitmap(input.dead));

    let mut out = header.to_vec();
    out.extend_from_slice(&data);
    out.extend_from_slice(&crc32(&data).to_le_bytes());
    Ok(out)
}

/// 编码 64 字节头部并计算 `header_crc32`(覆盖 `[0,32)`)。
fn encode_header(header: &VsecHeader) -> Result<[u8; HEADER_LEN as usize]> {
    let mut out = [0_u8; HEADER_LEN as usize];
    out[0..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    out[6..8].copy_from_slice(&HEADER_LEN.to_le_bytes());
    out[8..12].copy_from_slice(&header.dimension.to_le_bytes());
    out[12] = metric_to_u8(header.metric);
    out[13] = header.quant;
    out[14] = u8::from(header.norm_col);
    out[16..24].copy_from_slice(&header.row_count.to_le_bytes());
    out[24..32].copy_from_slice(&header.created_unix_ms.to_le_bytes());
    let crc = crc32(&out[0..32]);
    out[32..36].copy_from_slice(&crc.to_le_bytes());
    Ok(out)
}

/// 编码删除位图:每 1024 行一块,块内 16 个 `u64`;末块未用槽恒置 `1`。
fn encode_bitmap(dead: &[bool]) -> Vec<u8> {
    let blocks = dead.len().div_ceil(BITMAP_BLOCK_ROWS);
    let mut out = vec![0_u8; blocks * BITMAP_BLOCK_BYTES];
    for (row, &is_dead) in dead.iter().enumerate() {
        if is_dead {
            set_bit(&mut out, row);
        }
    }
    // 末块的行尾槽位置 1(不可见)。
    for row in dead.len()..(blocks * BITMAP_BLOCK_ROWS) {
        set_bit(&mut out, row);
    }
    out
}

/// 在删除位图缓冲中把第 `row` 位置 1。
fn set_bit(bitmap: &mut [u8], row: usize) {
    let block = row / BITMAP_BLOCK_ROWS;
    let within = row % BITMAP_BLOCK_ROWS;
    let word = within / 64;
    let bit = within % 64;
    let index = block * BITMAP_BLOCK_BYTES + word * 8;
    let mut value = u64::from_le_bytes([
        bitmap[index],
        bitmap[index + 1],
        bitmap[index + 2],
        bitmap[index + 3],
        bitmap[index + 4],
        bitmap[index + 5],
        bitmap[index + 6],
        bitmap[index + 7],
    ]);
    value |= 1_u64 << bit;
    bitmap[index..index + 8].copy_from_slice(&value.to_le_bytes());
}

/// 校验并解析向量段,返回对数据区的视图(不复制整个文件)。
///
/// # Errors
/// 魔数/版本/`header_len`/头部 CRC 不符、布局不一致或 `quant != 0` 时返回结构化错误。
pub(crate) fn parse(bytes: &[u8]) -> Result<VsecView<'_>> {
    let header = parse_header(bytes)?;
    let stride = row_stride(header.dimension);
    let vec_len = (header.row_count as usize)
        .checked_mul(stride)
        .ok_or_else(|| MnemeError::Corrupted {
            segment: None,
            reason: "vsec: 向量区长度溢出".to_string(),
        })?;
    let norm_len = if header.norm_col {
        (header.row_count as usize) * 4
    } else {
        0
    };
    let bitmap_len = bitmap_bytes(header.row_count);
    let expected = HEADER_LEN as usize + vec_len + norm_len + bitmap_len + 4;
    if bytes.len() != expected {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: format!("vsec: 文件长度 {} 应为 {expected}", bytes.len()),
        });
    }
    let data = &bytes[HEADER_LEN as usize..bytes.len() - 4];
    let payload_crc = u32::from_le_bytes([
        bytes[bytes.len() - 4],
        bytes[bytes.len() - 3],
        bytes[bytes.len() - 2],
        bytes[bytes.len() - 1],
    ]);
    Ok(VsecView {
        header,
        data,
        stride,
        vec_len,
        norm_len,
        payload_crc,
        payload_crc_ok: None,
    })
}

/// 校验并解析 vsec 定长头部。
fn parse_header(bytes: &[u8]) -> Result<VsecHeader> {
    let mut cursor = Cursor::new(bytes, "vsec 头部");
    if cursor.take(4)? != MAGIC {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "vsec: 魔数不符".to_string(),
        });
    }
    check_version("vsec", cursor.u16()?)?;
    let header_len = cursor.u16()?;
    if header_len != HEADER_LEN {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: format!("vsec: header_len={header_len} 非 {HEADER_LEN}"),
        });
    }
    if bytes.len() < HEADER_LEN as usize {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "vsec: 文件短于头部".to_string(),
        });
    }
    let stored_crc = u32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]);
    if crc32(&bytes[0..32]) != stored_crc {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "vsec: header_crc32 不符".to_string(),
        });
    }
    let dimension = cursor.u32()?;
    let metric = metric_from_u8(cursor.u8()?)?;
    let quant = cursor.u8()?;
    let norm_col = cursor.u8()? != 0;
    let _reserved = cursor.u8()?;
    let row_count = cursor.u64()?;
    let created_unix_ms = cursor.i64()?;
    if quant != 0 {
        return Err(MnemeError::Unsupported {
            feature: "量化副本(vsec quant != 0, L6)",
        });
    }
    Ok(VsecHeader {
        dimension,
        metric,
        quant,
        norm_col,
        row_count,
        created_unix_ms,
    })
}

/// 向量段只读视图(零拷贝)。
pub(crate) struct VsecView<'a> {
    header: VsecHeader,
    data: &'a [u8],
    stride: usize,
    vec_len: usize,
    norm_len: usize,
    payload_crc: u32,
    payload_crc_ok: Option<bool>,
}

impl VsecView<'_> {
    /// 头部。
    pub(crate) const fn header(&self) -> VsecHeader {
        self.header
    }

    /// 行数。
    pub(crate) const fn row_count(&self) -> u64 {
        self.header.row_count
    }

    /// 第 `row` 行的向量(逐元素小端解码);行越界返回 `None`。
    pub(crate) fn vector(&self, row: usize) -> Option<Vec<f32>> {
        if row >= self.header.row_count as usize {
            return None;
        }
        let start = row * self.stride;
        let dimension = self.header.dimension as usize;
        let mut vector = Vec::with_capacity(dimension);
        for col in 0..dimension {
            let index = start + col * 4;
            vector.push(f32::from_le_bytes([
                self.data[index],
                self.data[index + 1],
                self.data[index + 2],
                self.data[index + 3],
            ]));
        }
        Some(vector)
    }

    /// 第 `row` 行的范数平方;`norm_col = false` 或行越界时返回 `None`。
    pub(crate) fn norm_sq(&self, row: usize) -> Option<f32> {
        if !self.header.norm_col || row >= self.header.row_count as usize {
            return None;
        }
        let start = self.vec_len + row * 4;
        Some(f32::from_le_bytes([
            self.data[start],
            self.data[start + 1],
            self.data[start + 2],
            self.data[start + 3],
        ]))
    }

    /// 第 `row` 行是否不可见(删除位图置位)。
    pub(crate) fn is_dead(&self, row: usize) -> bool {
        if row >= self.header.row_count as usize {
            return true;
        }
        let bitmap = &self.data[self.vec_len + self.norm_len..];
        let block = row / BITMAP_BLOCK_ROWS;
        let within = row % BITMAP_BLOCK_ROWS;
        let word = within / 64;
        let bit = within % 64;
        let index = block * BITMAP_BLOCK_BYTES + word * 8;
        let value = u64::from_le_bytes([
            bitmap[index],
            bitmap[index + 1],
            bitmap[index + 2],
            bitmap[index + 3],
            bitmap[index + 4],
            bitmap[index + 5],
            bitmap[index + 6],
            bitmap[index + 7],
        ]);
        value & (1_u64 << bit) != 0
    }

    /// 校验数据区 payload CRC;结果被缓存以避免重复计算。
    ///
    /// # Errors
    /// CRC 不符时返回 [`MnemeError::Corrupted`]。
    pub(crate) fn verify_payload(&mut self) -> Result<()> {
        let ok = *self
            .payload_crc_ok
            .get_or_insert_with(|| crc32(self.data) == self.payload_crc);
        if ok {
            Ok(())
        } else {
            Err(MnemeError::Corrupted {
                segment: None,
                reason: "vsec: payload_crc32 不符".to_string(),
            })
        }
    }

    /// 数据区总字节数(向量 + norm + 位图)。
    pub(crate) const fn data_len(&self) -> usize {
        self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(count: usize) -> (Vec<Vec<f32>>, Vec<f32>, Vec<bool>) {
        let vectors: Vec<Vec<f32>> = (0..count)
            .map(|row| (0..4).map(|col| (row * 4 + col) as f32 * 0.5).collect())
            .collect();
        let norms = vectors
            .iter()
            .map(|v| v.iter().map(|x| x * x).sum())
            .collect();
        let dead = (0..count).map(|row| row % 2 == 1).collect();
        (vectors, norms, dead)
    }

    fn encode_sample(count: usize) -> Vec<u8> {
        let (vectors, norms, dead) = sample(count);
        let refs: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
        encode(&VsecInput {
            dimension: 4,
            metric: Metric::Cosine,
            created_unix_ms: 1_700_000_000_000,
            vectors: &refs,
            norms: &norms,
            dead: &dead,
        })
        .expect("encode")
    }

    /// 往返:头部字段、向量、范数与删除位图一致。
    #[test]
    fn vsec_roundtrip() {
        let bytes = encode_sample(3);
        let mut view = parse(&bytes).expect("parse");
        assert_eq!(view.row_count(), 3);
        assert_eq!(view.header().dimension, 4);
        assert_eq!(view.header().metric, Metric::Cosine);
        assert_eq!(view.header().created_unix_ms, 1_700_000_000_000);
        assert_eq!(view.vector(0).expect("row0"), vec![0.0, 0.5, 1.0, 1.5]);
        assert_eq!(view.vector(2).expect("row2"), vec![4.0, 4.5, 5.0, 5.5]);
        assert!(!view.is_dead(0));
        assert!(view.is_dead(1));
        assert!(!view.is_dead(2));
        view.verify_payload().expect("payload crc");
        assert!(view.vector(3).is_none());
    }

    /// 跨块(> 1024 行)删除位图仍按位正确。
    #[test]
    fn vsec_bitmap_spans_blocks() {
        let bytes = encode_sample(1025);
        let view = parse(&bytes).expect("parse");
        // 行 1023(奇数)不可见、行 1024(偶数)可见,跨块边界仍按位正确。
        assert!(view.is_dead(1023));
        assert!(!view.is_dead(1024));
        assert_eq!(view.row_count(), 1025);
    }

    /// 头部 CRC 被翻转后必须被检出。
    #[test]
    fn vsec_detects_header_corruption() {
        let mut bytes = encode_sample(1);
        bytes[8] ^= 0xFF;
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// payload CRC 被翻转后必须被检出。
    #[test]
    fn vsec_detects_payload_corruption() {
        let mut bytes = encode_sample(1);
        let last = bytes.len() - 5;
        bytes[last] ^= 0x01;
        let mut view = parse(&bytes).expect("parse");
        assert!(matches!(
            view.verify_payload(),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    /// 魔数不符 → `Corrupted`。
    #[test]
    fn vsec_rejects_bad_magic() {
        let mut bytes = encode_sample(1);
        bytes[0] = b'X';
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// 更高主版本 → `UnsupportedVersion`(I18)。
    #[test]
    fn vsec_rejects_higher_major() {
        let mut bytes = encode_sample(1);
        bytes[4..6].copy_from_slice(&0x0100_u16.to_le_bytes());
        // 重新计算头部 CRC,使版本成为唯一错误来源。
        let crc = crc32(&bytes[0..32]);
        bytes[32..36].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            parse(&bytes),
            Err(MnemeError::UnsupportedVersion { .. })
        ));
    }
}
