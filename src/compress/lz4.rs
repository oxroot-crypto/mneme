//! 内置 LZ4 风格块压缩(`compress/lz4.rs`,设计 11 §3)。
//!
//! 自研 LZ77 贪心匹配 + 序列编码(与 LZ4 块格式同族但独立定义,不承诺与
//! 外部 LZ4 实现互换):每个序列 `token = (字面量长度 << 4) | 匹配长度基线`,
//! 长度 ≥ 15 用 255 续字节;匹配偏移为 `u16` 小端(1..=65535),匹配长度加
//! 最小匹配 4。解码端以 `expected_len` 为上界,越界/畸形一律结构化拒绝。

use crate::core::error::{MnemeError, Result};

use super::Codec;

/// 最小匹配长度。
const MIN_MATCH: usize = 4;
/// 哈希表位数(2^14 桶,约 128 KiB;段级一次性分配)。
const HASH_BITS: u32 = 14;
/// 匹配偏移上限(u16)。
const MAX_OFFSET: usize = u16::MAX as usize;

/// 内置 LZ4 风格 codec 单例。
pub(crate) struct Lz4Codec;

impl Codec for Lz4Codec {
    fn compress(&self, src: &[u8]) -> Vec<u8> {
        compress(src)
    }

    fn decompress(&self, src: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        decompress(src, expected_len)
    }
}

/// 4 字节哈希(splitmix 常量乘法,分布足够且零依赖)。
fn hash(quad: &[u8]) -> usize {
    let value = u32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]);
    (value.wrapping_mul(0x9E37_79B9) >> (32 - HASH_BITS)) as usize
}

/// 写入长度扩展(调用方已写 token 的 15 基线)。
fn put_length(out: &mut Vec<u8>, mut len: usize) {
    while len >= 255 {
        out.push(255);
        len -= 255;
    }
    out.push(len as u8);
}

/// 写一个序列:字面量 + 匹配长度/偏移。
fn put_sequence(out: &mut Vec<u8>, literals: &[u8], offset: usize, match_len: usize) {
    let literal_nibble = literals.len().min(15) as u8;
    let match_nibble = (match_len - MIN_MATCH).min(15) as u8;
    out.push((literal_nibble << 4) | match_nibble);
    if literals.len() >= 15 {
        put_length(out, literals.len() - 15);
    }
    out.extend_from_slice(literals);
    out.extend_from_slice(&(offset as u16).to_le_bytes());
    if match_len - MIN_MATCH >= 15 {
        put_length(out, match_len - MIN_MATCH - 15);
    }
}

/// 压缩 `src`(贪心匹配;输出可与原文同长甚至更长,调用方按收益决定是否采用)。
fn compress(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    if src.len() < MIN_MATCH {
        put_last_literals(&mut out, src);
        return out;
    }
    let mut table = vec![0_usize; 1 << HASH_BITS];
    let mut anchor = 0_usize;
    let mut pos = 0_usize;
    while pos + MIN_MATCH <= src.len() {
        let slot = hash(&src[pos..pos + MIN_MATCH]);
        let candidate = table[slot];
        table[slot] = pos + 1;
        if candidate == 0 {
            pos += 1;
            continue;
        }
        let candidate = candidate - 1;
        if pos - candidate > MAX_OFFSET
            || src[candidate..candidate + MIN_MATCH] != src[pos..pos + MIN_MATCH]
        {
            pos += 1;
            continue;
        }
        let mut match_len = MIN_MATCH;
        while pos + match_len < src.len() && src[candidate + match_len] == src[pos + match_len] {
            match_len += 1;
        }
        put_sequence(&mut out, &src[anchor..pos], pos - candidate, match_len);
        pos += match_len;
        anchor = pos;
    }
    put_last_literals(&mut out, &src[anchor..]);
    out
}

/// 写末尾纯字面量序列(无匹配部分)。
fn put_last_literals(out: &mut Vec<u8>, literals: &[u8]) {
    let literal_nibble = literals.len().min(15) as u8;
    out.push(literal_nibble << 4);
    if literals.len() >= 15 {
        put_length(out, literals.len() - 15);
    }
    out.extend_from_slice(literals);
}

/// 构造 `lz4` 命名空间的损坏错误。
fn corrupt(reason: &str) -> MnemeError {
    MnemeError::Corrupted {
        segment: None,
        reason: format!("lz4: {reason}"),
    }
}

/// 读长度:token 的 4 位基线;基线为 15 时继续读 255 续字节。
///
/// `overflow` 为长度扩展越界时的错误描述(字面量/匹配长度两种调用口径)。
fn read_length(src: &[u8], index: &mut usize, base: usize, overflow: &str) -> Result<usize> {
    let mut len = base;
    if len != 15 {
        return Ok(len);
    }
    loop {
        let byte = *src.get(*index).ok_or_else(|| corrupt(overflow))?;
        *index += 1;
        len += usize::from(byte);
        if byte != 255 {
            break;
        }
    }
    Ok(len)
}

/// [`append_literals`] 的输入参数。
#[derive(Debug)]
struct AppendLiteralsInput<'a> {
    /// 输入字节流。
    src: &'a [u8],
    /// 解码游标(指向字面量段起点,函数内推进)。
    index: &'a mut usize,
    /// 字面量字节数。
    literal_len: usize,
    /// 输出缓冲。
    out: &'a mut Vec<u8>,
    /// 声明的输出总长度上界。
    expected_len: usize,
}

/// 拷贝字面量段并推进游标;越界或超过声明输出长度即拒绝。
fn append_literals(input: AppendLiteralsInput<'_>) -> Result<()> {
    let AppendLiteralsInput {
        src,
        index,
        literal_len,
        out,
        expected_len,
    } = input;
    let end = index
        .checked_add(literal_len)
        .filter(|&end| end <= src.len())
        .ok_or_else(|| corrupt("字面量越界"))?;
    if out.len() + literal_len > expected_len {
        return Err(corrupt("输出超过声明长度"));
    }
    out.extend_from_slice(&src[*index..end]);
    *index = end;
    Ok(())
}

/// [`append_match`] 的输入参数。
#[derive(Debug)]
struct AppendMatchInput<'a> {
    /// 输入字节流。
    src: &'a [u8],
    /// 解码游标(指向匹配偏移起点,函数内推进)。
    index: &'a mut usize,
    /// token 低 4 位的匹配长度基线。
    token_len: u8,
    /// 输出缓冲。
    out: &'a mut Vec<u8>,
    /// 声明的输出总长度上界。
    expected_len: usize,
}

/// 解码一个匹配段:偏移 + 匹配长度,逐字节复制以支持重叠匹配(LZ77 语义)。
fn append_match(input: AppendMatchInput<'_>) -> Result<()> {
    let AppendMatchInput {
        src,
        index,
        token_len,
        out,
        expected_len,
    } = input;
    let offset_bytes = src
        .get(*index..*index + 2)
        .ok_or_else(|| corrupt("匹配偏移越界"))?;
    *index += 2;
    let offset = usize::from(u16::from_le_bytes([offset_bytes[0], offset_bytes[1]]));
    if offset == 0 || offset > out.len() {
        return Err(corrupt("匹配偏移非法"));
    }
    let match_len =
        read_length(src, index, usize::from(token_len), "匹配长度扩展越界")? + MIN_MATCH;
    if out.len() + match_len > expected_len {
        return Err(corrupt("匹配超过声明长度"));
    }
    // 逐字节复制以支持重叠匹配(LZ77 语义:可引用本次匹配刚写出的字节)。
    for _ in 0..match_len {
        let byte = out[out.len() - offset];
        out.push(byte);
    }
    Ok(())
}

/// 解压 `src` 为恰好 `expected_len` 字节。
fn decompress(src: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    // 预分配有界:不信任 expected 的极端值(entry 层另有上限)。
    let mut out: Vec<u8> = Vec::with_capacity(expected_len.min(1 << 20));
    let mut index = 0_usize;
    loop {
        let token = *src
            .get(index)
            .ok_or_else(|| corrupt("流提前结束(缺 token)"))?;
        index += 1;
        let literal_len = read_length(
            src,
            &mut index,
            usize::from(token >> 4),
            "字面量长度扩展越界",
        )?;
        append_literals(AppendLiteralsInput {
            src,
            index: &mut index,
            literal_len,
            out: &mut out,
            expected_len,
        })?;
        if index == src.len() {
            break;
        }
        append_match(AppendMatchInput {
            src,
            index: &mut index,
            token_len: token & 0x0F,
            out: &mut out,
            expected_len,
        })?;
    }
    if out.len() != expected_len {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: format!("lz4: 输出长度 {} 与期望 {expected_len} 不符", out.len()),
        });
    }
    Ok(out)
}
