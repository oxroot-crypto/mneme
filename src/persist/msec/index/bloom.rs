//! msec bloom 区编解码。

use crate::core::error::Result;
use crate::memory::analysis::BloomSet;
use crate::persist::{Cursor, put_u16, put_u32, put_u64};

use super::common::{MAX_FIELDS, corrupted};

/// 编码 bloom 区(当前仅 `key` 字段一个布隆过滤器)。
pub(crate) fn encode_bloom(bloom: &BloomSet, field_id: u16) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, 1);
    put_u16(&mut out, field_id);
    put_u32(&mut out, bloom.bit_len() as u32);
    put_u32(&mut out, bloom.hash_count() as u32);
    for word in bloom.words() {
        put_u64(&mut out, *word);
    }
    out
}

/// 解码 bloom 区;结构非法(位数/字数/越界)返回 `Corrupted`。
pub(crate) fn decode_bloom(bytes: &[u8]) -> Result<Vec<(u16, BloomSet)>> {
    let mut cursor = Cursor::new(bytes, "msec bloom");
    let count = cursor.u32()? as usize;
    if count > MAX_FIELDS {
        return Err(corrupted("bloom: 过滤器数量超上限"));
    }
    let mut blooms = Vec::new();
    for _ in 0..count {
        let field_id = cursor.u16()?;
        let bit_len = cursor.u32()? as usize;
        let k = cursor.u32()? as usize;
        if bit_len == 0 || !bit_len.is_multiple_of(64) {
            return Err(corrupted("bloom: bit_len 非法"));
        }
        let mut words = Vec::new();
        for _ in 0..bit_len / 64 {
            words.push(cursor.u64()?);
        }
        blooms.push((field_id, BloomSet::from_words(bit_len, k, words)?));
    }
    if !cursor.is_empty() {
        return Err(corrupted("bloom: 尾部有残留字节"));
    }
    Ok(blooms)
}
