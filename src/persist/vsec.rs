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
const MAX_DIMENSION: u32 = 65536;
/// 删除位图分块行数。
const BITMAP_BLOCK_ROWS: usize = 1024;
/// 删除位图单块字节数(16 × u64)。
const BITMAP_BLOCK_BYTES: usize = 128;
/// i8 逐维参数表单维字节数(`v_min` + `v_max`)。
const I8_PARAMS_PER_DIM: usize = 8;
/// f16 单分量字节数。
const F16_BYTES_PER_ELEMENT: usize = 2;

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
fn quant_len(header: &VsecHeader) -> Result<usize> {
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
    let data = encode_data(input, row_stride(input.dimension), quant_bytes);

    let mut out = header.to_vec();
    out.extend_from_slice(&data);
    out.extend_from_slice(&crc32(&data).to_le_bytes());
    Ok(out)
}

/// 编码数据区(向量区 + norm 区 + qvec 区 + 删除位图),不含头与尾 CRC。
fn encode_data(input: &VsecInput<'_>, stride: usize, quant_bytes: usize) -> Vec<u8> {
    let count = input.vectors.len();
    let mut data =
        Vec::with_capacity(count * stride + count * 4 + quant_bytes + bitmap_bytes(count as u64));
    for vector in input.vectors {
        for value in *vector {
            data.extend_from_slice(&value.to_le_bytes());
        }
        data.resize(data.len() + (stride - vector.len() * 4), 0);
    }
    for norm in input.norms {
        data.extend_from_slice(&norm.to_le_bytes());
    }
    for param in input.quant_params {
        data.extend_from_slice(&param.to_le_bytes());
    }
    for row in input.quant_codes {
        data.extend_from_slice(row);
    }
    data.extend_from_slice(&encode_bitmap(input.dead));
    data
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

/// 编码 64 字节头部并计算 `header_crc32`(覆盖 `[0,32)`)。
fn encode_header(header: &VsecHeader) -> Result<[u8; HEADER_LEN as usize]> {
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

/// 构造 vsec 文件级损坏错误。
fn corrupt(reason: &str) -> MnemeError {
    MnemeError::Corrupted {
        segment: None,
        reason: format!("vsec: {reason}"),
    }
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

    /// i8 逐维 `(v_min, v_max)` 交错表;非 i8 返回空表。
    pub(crate) fn quant_params(&self) -> Vec<f32> {
        let (start, len) = self.params_range;
        self.data[start..start + len]
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect()
    }

    /// 第 `row` 行的量化码流;无副本或行越界返回 `None`。
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
            quant: VectorFormat::F32,
            quant_params: &[],
            quant_codes: &[],
        })
        .expect("encode")
    }

    /// i8 qvec 样本:参数表按列统计构造,码流逐行编码。
    fn encode_i8_sample(count: usize) -> Vec<u8> {
        let (vectors, norms, dead) = sample(count);
        let refs: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
        let params = crate::quant::scalar_i8::build_params(&refs, 4).expect("build_params");
        let codes: Vec<Vec<u8>> = vectors
            .iter()
            .map(|vector| crate::quant::scalar_i8::encode_row(vector, &params))
            .collect();
        let code_refs: Vec<&[u8]> = codes.iter().map(Vec::as_slice).collect();
        encode(&VsecInput {
            dimension: 4,
            metric: Metric::Cosine,
            created_unix_ms: 1_700_000_000_000,
            vectors: &refs,
            norms: &norms,
            dead: &dead,
            quant: VectorFormat::I8Rescored,
            quant_params: &params.table(),
            quant_codes: &code_refs,
        })
        .expect("encode")
    }

    /// 往返:头部字段、向量、范数与删除位图一致。
    #[test]
    fn vsec_roundtrip() {
        let bytes = encode_sample(3);
        let mut view = parse(&bytes).expect("parse");
        assert_eq!(view.row_count(), 3);
        assert_eq!(view.quant(), VectorFormat::F32);
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

    /// FC-QUANT-POST-002:i8 qvec 往返(参数表逐位一致、码流逐行一致)。
    #[test]
    fn vsec_i8_quantized_roundtrip() {
        let bytes = encode_i8_sample(3);
        let mut view = parse(&bytes).expect("parse");
        assert_eq!(view.quant(), VectorFormat::I8Rescored);
        let (vectors, _, _) = sample(3);
        let refs: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
        let params = crate::quant::scalar_i8::build_params(&refs, 4).expect("build_params");
        assert_eq!(view.quant_params(), params.table());
        for (row, vector) in vectors.iter().enumerate() {
            let expected = crate::quant::scalar_i8::encode_row(vector, &params);
            assert_eq!(view.quant_row(row).expect("quant row"), expected.as_slice());
        }
        assert!(view.quant_row(3).is_none());
        view.verify_payload().expect("payload crc");
    }

    /// FC-QUANT-ERR-003:未知 quant 编码 → `Corrupted`。
    #[test]
    fn vsec_rejects_unknown_quant_code() {
        let mut bytes = encode_sample(1);
        bytes[13] = 9;
        let crc = crc32(&bytes[0..32]);
        bytes[32..36].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// FC-QUANT-ERR-003:i8 参数表非有限值 → `Corrupted`。
    #[test]
    fn vsec_rejects_malformed_i8_params() {
        let mut bytes = encode_i8_sample(2);
        // 参数表起始 = 64(头) + 2 行 × 32B(vec 区) + 2 行 × 4B(norm 区)。
        let params_start = 64 + 2 * 32 + 2 * 4;
        bytes[params_start..params_start + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        let payload_crc = crc32(&bytes[64..bytes.len() - 4]);
        let tail = bytes.len() - 4;
        bytes[tail..].copy_from_slice(&payload_crc.to_le_bytes());
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// FC-QUANT-ERR-003:文件长度与头部布局不符(截断/多余字节)→ `Corrupted`,
    /// 绝不按短读部分解析。
    #[test]
    fn vsec_rejects_length_mismatch() {
        let bytes = encode_sample(2);
        assert!(matches!(
            parse(&bytes[..bytes.len() - 1]),
            Err(MnemeError::Corrupted { .. })
        ));
        let mut longer = bytes;
        longer.push(0);
        assert!(matches!(parse(&longer), Err(MnemeError::Corrupted { .. })));
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

    /// 更高/更低版本 → `UnsupportedVersion`(I18)。
    #[test]
    fn vsec_rejects_version_mismatch() {
        // 高/低版本都必须拒绝(精确匹配,无旧格式兼容)。
        for version in [0x0100_u16, crate::persist::FORMAT_VERSION - 1] {
            let mut bytes = encode_sample(1);
            bytes[4..6].copy_from_slice(&version.to_le_bytes());
            // 重新计算头部 CRC,使版本成为唯一错误来源。
            let crc = crc32(&bytes[0..32]);
            bytes[32..36].copy_from_slice(&crc.to_le_bytes());
            assert!(matches!(
                parse(&bytes),
                Err(MnemeError::UnsupportedVersion { .. })
            ));
        }
    }

    /// f16 副本(仅 feature `quant-f16`)往返;关闭 feature 时编码被拒绝。
    #[cfg(feature = "quant-f16")]
    #[test]
    fn vsec_f16_quantized_roundtrip() {
        let (vectors, norms, dead) = sample(2);
        let refs: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
        let codes: Vec<Vec<u8>> = vectors
            .iter()
            .map(|vector| crate::quant::f16::encode_row(vector))
            .collect();
        let code_refs: Vec<&[u8]> = codes.iter().map(Vec::as_slice).collect();
        let bytes = encode(&VsecInput {
            dimension: 4,
            metric: Metric::Cosine,
            created_unix_ms: 0,
            vectors: &refs,
            norms: &norms,
            dead: &dead,
            quant: VectorFormat::F16,
            quant_params: &[],
            quant_codes: &code_refs,
        })
        .expect("encode");
        let view = parse(&bytes).expect("parse");
        assert_eq!(view.quant(), VectorFormat::F16);
        assert_eq!(view.quant_row(0).expect("row0"), codes[0].as_slice());
        assert_eq!(view.quant_row(1).expect("row1"), codes[1].as_slice());
        assert!(view.quant_params().is_empty());
    }

    /// FC-QUANT-ERR-001:未开 feature 时编码 f16 副本必须报 `Unsupported`。
    #[cfg(not(feature = "quant-f16"))]
    #[test]
    fn vsec_f16_encode_requires_feature() {
        let (vectors, norms, dead) = sample(1);
        let refs: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
        let row = [0_u8; 8];
        let codes: [&[u8]; 1] = [&row];
        let error = encode(&VsecInput {
            dimension: 4,
            metric: Metric::Cosine,
            created_unix_ms: 0,
            vectors: &refs,
            norms: &norms,
            dead: &dead,
            quant: VectorFormat::F16,
            quant_params: &[],
            quant_codes: &codes,
        })
        .expect_err("f16 编码应被 feature 门控拒绝");
        assert!(matches!(
            error,
            MnemeError::Unsupported {
                feature: "quant-f16"
            }
        ));
    }
}
