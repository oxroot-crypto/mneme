//! 向量段头部字段定义同度量/量化编码映射。
//!
//! 只管 64 字节定长头同量化副本区长度个换算;数据区布局见 [`super::view`],
//! 编码流程见 [`super::encode`]。

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::VectorFormat;
use crate::persist::{Cursor, FORMAT_VERSION, align_up, check_version, crc32};

/// 向量段魔数。
pub(crate) const MAGIC: [u8; 4] = *b"VSC1";
/// 定长头部长度(字节)。
pub(crate) const HEADER_LEN: u16 = 64;
/// 每行向量的对齐粒度(字节)。
const ROW_ALIGN: usize = 32;
/// 建库维度定义域上界(FC-CORE-PRE-001)。
pub(super) const MAX_DIMENSION: u32 = 65536;
/// 删除位图分块行数。
pub(super) const BITMAP_BLOCK_ROWS: usize = 1024;
/// 删除位图单块字节数(16 × u64)。
pub(super) const BITMAP_BLOCK_BYTES: usize = 128;
/// i8 逐维参数表单维字节数(`v_min` + `v_max`)。
pub(super) const I8_PARAMS_PER_DIM: usize = 8;
/// f16 单分量字节数。
pub(super) const F16_BYTES_PER_ELEMENT: usize = 2;

/// 向量段头部字段。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VsecHeader {
    /// 建库维度。
    pub(crate) dimension: u32,
    /// 距离度量。
    pub(crate) metric: Metric,
    /// 量化副本格式(`F32` = 无副本)。
    pub(crate) quant: VectorFormat,
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
    /// 量化副本格式(`F32` = 不写 qvec)。
    pub(crate) quant: VectorFormat,
    /// i8 的逐维 `(v_min, v_max)` 交错表;其余格式为空。
    pub(crate) quant_params: &'a [f32],
    /// 量化副本码流(按行;`F32` 时为空)。
    pub(crate) quant_codes: &'a [&'a [u8]],
}

/// 行跨距:向量字节数向上对齐到 32B。
pub(super) const fn row_stride(dimension: u32) -> usize {
    align_up(dimension as usize * std::mem::size_of::<f32>(), ROW_ALIGN)
}

/// 删除位图总字节数(每 1024 行一块、每块 128 B)。
pub(super) const fn bitmap_bytes(row_count: u64) -> usize {
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

/// 量化格式 → `quant` 字节编码(0=F32 1=i8 2=f16)。
pub(crate) const fn quant_to_u8(format: VectorFormat) -> u8 {
    match format {
        VectorFormat::F32 => 0,
        VectorFormat::I8Rescored => 1,
        VectorFormat::F16 => 2,
    }
}

/// `quant` 字节编码 → 量化格式;未知编码返回 `Corrupted`(FC-QUANT-ERR-003)。
pub(crate) fn quant_from_u8(value: u8) -> Result<VectorFormat> {
    match value {
        0 => Ok(VectorFormat::F32),
        1 => Ok(VectorFormat::I8Rescored),
        2 => Ok(VectorFormat::F16),
        _ => Err(MnemeError::Corrupted {
            segment: None,
            reason: format!("vsec: 未知量化编码 {value}"),
        }),
    }
}

/// qvec 区字节数(参数表 + 码流);`F32` 为 0。
pub(super) fn quant_len(header: &VsecHeader) -> Result<usize> {
    let rows = header.row_count as usize;
    let dimension = header.dimension as usize;
    match header.quant {
        VectorFormat::F32 => Ok(0),
        VectorFormat::I8Rescored => dimension
            .checked_mul(I8_PARAMS_PER_DIM)
            .and_then(|params| {
                rows.checked_mul(dimension)
                    .and_then(|codes| params.checked_add(codes))
            })
            .ok_or_else(|| corrupt("qvec 区长度溢出")),
        VectorFormat::F16 => rows
            .checked_mul(dimension)
            .and_then(|value| value.checked_mul(F16_BYTES_PER_ELEMENT))
            .ok_or_else(|| corrupt("qvec 区长度溢出")),
    }
}

/// 编码 64 字节头部并计算 `header_crc32`(覆盖 `[0,32)`)。
pub(super) fn encode_header(header: &VsecHeader) -> Result<[u8; HEADER_LEN as usize]> {
    let mut out = [0_u8; HEADER_LEN as usize];
    out[0..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    out[6..8].copy_from_slice(&HEADER_LEN.to_le_bytes());
    out[8..12].copy_from_slice(&header.dimension.to_le_bytes());
    out[12] = metric_to_u8(header.metric);
    out[13] = quant_to_u8(header.quant);
    out[14] = u8::from(header.norm_col);
    out[16..24].copy_from_slice(&header.row_count.to_le_bytes());
    out[24..32].copy_from_slice(&header.created_unix_ms.to_le_bytes());
    let crc = crc32(&out[0..32]);
    out[32..36].copy_from_slice(&crc.to_le_bytes());
    Ok(out)
}

/// 构造 vsec 文件级损坏错误。
pub(super) fn corrupt(reason: &str) -> MnemeError {
    MnemeError::Corrupted {
        segment: None,
        reason: format!("vsec: {reason}"),
    }
}

/// 校验并解析 vsec 定长头部。
pub(super) fn parse_header(bytes: &[u8]) -> Result<VsecHeader> {
    let mut cursor = Cursor::new(bytes, "vsec 头部");
    if cursor.take(4)? != MAGIC {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "vsec: 魔数不符".to_string(),
        });
    }
    check_version("vsec", cursor.u16()?, FORMAT_VERSION)?;
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
    let quant = quant_from_u8(cursor.u8()?)?;
    let norm_col = cursor.u8()? != 0;
    let _reserved = cursor.u8()?;
    let row_count = cursor.u64()?;
    let created_unix_ms = cursor.i64()?;
    Ok(VsecHeader {
        dimension,
        metric,
        quant,
        norm_col,
        row_count,
        created_unix_ms,
    })
}
