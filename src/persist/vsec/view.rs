//! 向量段解析:文件级校验 + 零拷贝 `VsecView` 数据区视图。
//!
//! 解析期即拒绝魔数/版本/长度/CRC/未知编码同畸形 i8 参数表(FC-QUANT-ERR-003);
//! 布局计算全走 checked 运算,免得长度回绕。

use crate::core::error::{MnemeError, Result};
use crate::core::options::VectorFormat;
use crate::persist::crc32;

#[cfg(test)]
use super::header::{BITMAP_BLOCK_BYTES, BITMAP_BLOCK_ROWS};
use super::header::{
    F16_BYTES_PER_ELEMENT, HEADER_LEN, I8_PARAMS_PER_DIM, MAX_DIMENSION, VsecHeader, bitmap_bytes,
    corrupt, parse_header, quant_len, row_stride,
};

/// 数据区布局:向量步长、i8 参数表区间(起,长)、量化码区起、单行码长。
struct VsecLayout {
    stride: usize,
    params_range: (usize, usize),
    codes_offset: usize,
    code_stride: usize,
}

/// 由头部计算数据区布局。
///
/// 文件总长已由 [`expected_file_len`] 以 checked 乘法校验,故此处无回绕风险。
fn layout(header: &VsecHeader) -> VsecLayout {
    let dimension = header.dimension as usize;
    let rows = header.row_count as usize;
    let stride = row_stride(header.dimension);
    let vec_len = rows * stride;
    let norm_len = if header.norm_col { rows * 4 } else { 0 };
    let params_len = if header.quant == VectorFormat::I8Rescored {
        dimension * I8_PARAMS_PER_DIM
    } else {
        0
    };
    let code_stride = match header.quant {
        VectorFormat::F32 => 0,
        VectorFormat::I8Rescored => dimension,
        VectorFormat::F16 => dimension * F16_BYTES_PER_ELEMENT,
    };
    let params_start = vec_len + norm_len;
    VsecLayout {
        stride,
        params_range: (params_start, params_len),
        codes_offset: params_start + params_len,
        code_stride,
    }
}

/// 校验并解析向量段,返回对数据区的视图(不复制整个文件)。
///
/// # Errors
/// 魔数/版本/`header_len`/头部 CRC 不符、布局不一致、未知量化编码或
/// i8 参数表畸形时返回结构化错误。
pub(crate) fn parse(bytes: &[u8]) -> Result<VsecView<'_>> {
    let header = parse_header(bytes)?;
    // 维度必须落在建库定义域 [1, 65536](FC-CORE-PRE-001);损坏文件可能为 0/超大值。
    if !(1..=MAX_DIMENSION).contains(&header.dimension) {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: format!("vsec: 维度 {} 越界", header.dimension),
        });
    }
    let expected = expected_file_len(&header)?;
    if bytes.len() != expected {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: format!("vsec: 文件长度 {} 应为 {expected}", bytes.len()),
        });
    }
    let layout = layout(&header);
    let data = &bytes[HEADER_LEN as usize..bytes.len() - 4];
    let payload_crc = u32::from_le_bytes([
        bytes[bytes.len() - 4],
        bytes[bytes.len() - 3],
        bytes[bytes.len() - 2],
        bytes[bytes.len() - 1],
    ]);
    let view = VsecView {
        header,
        data,
        stride: layout.stride,
        payload_crc,
        payload_crc_ok: None,
        params_range: layout.params_range,
        codes_offset: layout.codes_offset,
        code_stride: layout.code_stride,
    };
    // i8 参数表畸形(失序/非有限/长度不符)在解析期即拒绝(FC-QUANT-ERR-003)。
    if header.quant == VectorFormat::I8Rescored {
        let table = view.quant_params();
        crate::quant::scalar_i8::I8Params::from_table(&table, header.dimension as usize)?;
    }
    Ok(view)
}

/// 计算 vsec 文件的期望总长度:头 + 向量区 + norm 区 + qvec 区 + 位图 + 尾 CRC。
///
/// 全用 checked 运算:损坏的 `row_count`/`dimension` 不得让长度回绕而绕过校验。
///
/// # Errors
/// 任一分量溢出 `usize` 时返回 [`MnemeError::Corrupted`]。
fn expected_file_len(header: &VsecHeader) -> Result<usize> {
    let rows = header.row_count as usize;
    let vec_len = rows
        .checked_mul(row_stride(header.dimension))
        .ok_or_else(|| corrupt("向量区长度溢出"))?;
    let norm_len = if header.norm_col {
        rows.checked_mul(4)
            .ok_or_else(|| corrupt("norm 区长度溢出"))?
    } else {
        0
    };
    let quant_len = quant_len(header)?;
    (HEADER_LEN as usize)
        .checked_add(vec_len)
        .and_then(|value| value.checked_add(norm_len))
        .and_then(|value| value.checked_add(quant_len))
        .and_then(|value| value.checked_add(bitmap_bytes(header.row_count)))
        .and_then(|value| value.checked_add(4))
        .ok_or_else(|| corrupt("文件期望长度溢出"))
}

/// 向量段只读视图(零拷贝)。
pub(crate) struct VsecView<'a> {
    header: VsecHeader,
    data: &'a [u8],
    stride: usize,
    payload_crc: u32,
    payload_crc_ok: Option<bool>,
    /// i8 参数表在 `data` 中的 `(起始, 字节长度)`;非 i8 为 `(0, 0)`。
    params_range: (usize, usize),
    /// 码流在 `data` 中的起始偏移。
    codes_offset: usize,
    /// 每行码流字节数;无副本为 0。
    code_stride: usize,
}

impl VsecView<'_> {
    /// 头部(仅测试用;运行时经 [`VsecView::row_count`] / [`VsecView::vector`])。
    #[cfg(test)]
    pub(crate) const fn header(&self) -> VsecHeader {
        self.header
    }

    /// 量化副本格式(`F32` = 无副本)。
    pub(crate) const fn quant(&self) -> VectorFormat {
        self.header.quant
    }

    /// 行数。
    pub(crate) const fn row_count(&self) -> u64 {
        self.header.row_count
    }

    /// 维度(建库锁定,1..=65536)。
    pub(crate) const fn dimension(&self) -> u32 {
        self.header.dimension
    }

    /// 第 `row` 行向量在文件内的绝对字节偏移(供惰性句柄按需解码;
    /// 行区定长 `stride`,越界返回 `None`)。
    pub(crate) fn vector_offset(&self, row: usize) -> Option<usize> {
        if row >= self.header.row_count as usize {
            return None;
        }
        Some(HEADER_LEN as usize + row * self.stride)
    }

    /// 第 `row` 行的范数平方(`norm_col` 区;无范数列或越界返回 `None`)。
    ///
    /// 范数列存储的是编码时的 `norm_sq`(与 [`VsecView::vector`] 逐位对应),
    /// 读路径可直接取用而无需解码整行向量(FC-PERSIST-INV-021)。
    pub(crate) fn norm_sq(&self, row: usize) -> Option<f32> {
        if !self.header.norm_col || row >= self.header.row_count as usize {
            return None;
        }
        let rows = self.header.row_count as usize;
        let offset = rows * self.stride + row * size_of::<f32>();
        let bytes = self.data.get(offset..offset + size_of::<f32>())?;
        Some(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// 量化码区在文件内的 `(绝对偏移, 单行字节数)`;无副本(`F32`)返回 `None`。
    pub(crate) fn quant_region(&self) -> Option<(usize, usize)> {
        (self.code_stride > 0)
            .then_some((HEADER_LEN as usize + self.codes_offset, self.code_stride))
    }

    /// i8 逐维 `(v_min, v_max)` 交错表;非 i8 返回空表。
    pub(crate) fn quant_params(&self) -> Vec<f32> {
        let (start, len) = self.params_range;
        self.data[start..start + len]
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect()
    }

    /// 第 `row` 行的量化码流;无副本或行越界返回 `None`。
    // reason: 生产路径改走 `LazyRows` 惰性行区;逐行切片接口供测试与诊断。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn quant_row(&self, row: usize) -> Option<&[u8]> {
        if self.code_stride == 0 || row >= self.header.row_count as usize {
            return None;
        }
        let start = self.codes_offset + row * self.code_stride;
        Some(&self.data[start..start + self.code_stride])
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

    /// 第 `row` 行是否不可见(删除位图置位,仅测试用)。
    #[cfg(test)]
    pub(crate) fn is_dead(&self, row: usize) -> bool {
        if row >= self.header.row_count as usize {
            return true;
        }
        let rows = self.header.row_count as usize;
        let vec_len = rows * self.stride;
        let norm_len = if self.header.norm_col { rows * 4 } else { 0 };
        let bitmap =
            &self.data[vec_len + norm_len + self.params_range.1 + rows * self.code_stride..];
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
}
