//! 向量段编码:头部同数据区一次预留、按偏移写入,尾后附 payload CRC。
//!
//! 编码前校验 qvec 输入同格式/行数是否匹配;f16 个 feature 门控在
//! [`validate_f16_input`] 里,头部定义看 [`super::header`]。

use crate::core::error::{MnemeError, Result};
use crate::core::options::VectorFormat;
use crate::persist::crc32;

#[cfg(feature = "quant-f16")]
use super::header::F16_BYTES_PER_ELEMENT;
use super::header::{
    BITMAP_BLOCK_BYTES, BITMAP_BLOCK_ROWS, VsecHeader, VsecInput, bitmap_bytes, encode_header,
    quant_len, row_stride,
};

/// 编码一个完整的向量段文件。
///
/// # Errors
/// 输入不一致(行数/维度不匹配、qvec 长度不符)或维度非法时返回结构化错误;
/// 未开 `quant-f16` feature 时拒绝编码 f16 副本(FC-QUANT-ERR-001)。
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
    validate_quant_input(input, count)?;

    let header = VsecHeader {
        dimension: input.dimension,
        metric: input.metric,
        quant: input.quant,
        norm_col: true,
        row_count: count as u64,
        created_unix_ms: input.created_unix_ms,
    };
    let quant_bytes = quant_len(&header)?;
    let header = encode_header(&header)?;
    let stride = row_stride(input.dimension);
    let data_len = count * stride + count * 4 + quant_bytes + bitmap_bytes(count as u64);
    // 单缓冲:头与数据一次预留、按偏移写入,免「数据区物化后再整段拷入输出」。
    let mut out = Vec::with_capacity(header.len() + data_len + 4);
    out.extend_from_slice(&header);
    encode_data_into(&mut out, input, stride);
    let crc = crc32(&out[header.len()..]);
    out.extend_from_slice(&crc.to_le_bytes());
    Ok(out)
}

/// 追加编码数据区(向量区 + norm 区 + qvec 区 + 删除位图),不含头与尾 CRC。
fn encode_data_into(out: &mut Vec<u8>, input: &VsecInput<'_>, stride: usize) {
    for vector in input.vectors {
        for value in *vector {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.resize(out.len() + (stride - vector.len() * 4), 0);
    }
    for norm in input.norms {
        out.extend_from_slice(&norm.to_le_bytes());
    }
    for param in input.quant_params {
        out.extend_from_slice(&param.to_le_bytes());
    }
    for row in input.quant_codes {
        out.extend_from_slice(row);
    }
    out.extend_from_slice(&encode_bitmap(input.dead));
}

/// 校验 qvec 输入与格式/行数匹配(FC-QUANT-ERR-001/003 的编码侧防线)。
fn validate_quant_input(input: &VsecInput<'_>, count: usize) -> Result<()> {
    let dimension = input.dimension as usize;
    match input.quant {
        VectorFormat::F32 => validate_f32_input(input),
        VectorFormat::I8Rescored => validate_i8_input(input, count, dimension),
        VectorFormat::F16 => validate_f16_input(input, count, dimension),
    }
}

/// 码流行数与单行长度是否匹配。
fn codes_match(input: &VsecInput<'_>, count: usize, stride: usize) -> bool {
    input.quant_codes.len() == count && input.quant_codes.iter().all(|row| row.len() == stride)
}

/// `F32` 不得携带量化副本。
fn validate_f32_input(input: &VsecInput<'_>) -> Result<()> {
    if !input.quant_params.is_empty() || !input.quant_codes.is_empty() {
        return Err(MnemeError::Config {
            reason: "vsec 编码:F32 不得携带量化副本",
        });
    }
    Ok(())
}

/// i8 副本长度与参数表定义域校验。
fn validate_i8_input(input: &VsecInput<'_>, count: usize, dimension: usize) -> Result<()> {
    if input.quant_params.len() != dimension * 2 || !codes_match(input, count, dimension) {
        return Err(MnemeError::Config {
            reason: "vsec 编码:i8 副本长度与维度/行数不符",
        });
    }
    // 与解析侧 `I8Params::from_table` 对称:编码侧也不得写出非有限/失序表。
    for pair in input.quant_params.chunks_exact(2) {
        if !pair[0].is_finite() || !pair[1].is_finite() || pair[0] > pair[1] {
            return Err(MnemeError::Config {
                reason: "vsec 编码:i8 参数表含非有限值或失序",
            });
        }
    }
    Ok(())
}

/// f16 副本长度与 feature 门控校验。
fn validate_f16_input(input: &VsecInput<'_>, count: usize, dimension: usize) -> Result<()> {
    #[cfg(not(feature = "quant-f16"))]
    {
        let _ = (input, count, dimension);
        Err(MnemeError::Unsupported {
            feature: "quant-f16",
        })
    }
    #[cfg(feature = "quant-f16")]
    {
        if !input.quant_params.is_empty()
            || !codes_match(input, count, dimension * F16_BYTES_PER_ELEMENT)
        {
            Err(MnemeError::Config {
                reason: "vsec 编码:f16 副本长度与维度/行数不符",
            })
        } else {
            Ok(())
        }
    }
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
