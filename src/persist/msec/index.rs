//! msec 轻量索引区编解码(`msec/index.rs`,设计 04 §2.2、§5)。
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

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::memory::analysis::{BloomSet, ZoneIndex, ZoneKind};
use crate::persist::{Cursor, put_bytes_u32, put_u16, put_u32, put_u64};

/// 字段字典条目数硬上限(id 为 `u16`,防御恶意文件声明巨量条目)。
const MAX_FIELDS: usize = u16::MAX as usize + 1;

/// 字段类别(落盘值固定,新增需递增次版本)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldKind {
    /// 普通数值。
    Num = 0,
    /// Unix 毫秒时间戳。
    Ts = 1,
    /// 字符串(仅 bloom 预筛,无 zone map)。
    Str = 2,
}

impl FieldKind {
    /// 由存储字节还原;未知值返回 `Corrupted`。
    fn from_u8(value: u8) -> Result<Self> {
        match value {
            0 => Ok(FieldKind::Num),
            1 => Ok(FieldKind::Ts),
            2 => Ok(FieldKind::Str),
            _ => Err(corrupted("field_dict: 未知字段类别")),
        }
    }

    /// 由 zone 字段类别映射。
    pub(crate) fn from_zone(kind: ZoneKind) -> Self {
        match kind {
            ZoneKind::Num => FieldKind::Num,
            ZoneKind::Ts => FieldKind::Ts,
        }
    }
}

/// 解码后的字段字典条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldDef {
    /// 字段编号(与区索引一致)。
    pub(crate) id: u16,
    /// 字段类别。
    pub(crate) kind: FieldKind,
    /// 字段名(点路径)。
    pub(crate) name: Arc<str>,
}

/// 构造结构损坏错误。
pub(super) fn corrupted(reason: &str) -> MnemeError {
    MnemeError::Corrupted {
        segment: None,
        reason: reason.to_string(),
    }
}

/// 读取带 `u32` 长度前缀的 UTF-8 字符串。
pub(super) fn read_utf8(cursor: &mut Cursor<'_>, what: &str) -> Result<Arc<str>> {
    let len = cursor.u32()? as usize;
    let text = std::str::from_utf8(cursor.take(len)?)
        .map_err(|_| corrupted(&format!("{what}: 非法 UTF-8")))?;
    Ok(Arc::from(text))
}

/// 编码字段字典。
pub(crate) fn encode_field_dict(fields: &[(Arc<str>, FieldKind)]) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, fields.len() as u32);
    for (id, (name, kind)) in fields.iter().enumerate() {
        put_u16(&mut out, id as u16);
        out.push(*kind as u8);
        put_bytes_u32(&mut out, name.as_bytes());
    }
    out
}

/// 解码字段字典(校验条目数、编号连续与类别合法)。
pub(crate) fn decode_field_dict(bytes: &[u8]) -> Result<Vec<FieldDef>> {
    let mut cursor = Cursor::new(bytes, "msec field_dict");
    let count = cursor.u32()? as usize;
    if count > MAX_FIELDS {
        return Err(corrupted("field_dict: 条目数超上限"));
    }
    let mut fields = Vec::new();
    for expected_id in 0..count {
        let id = cursor.u16()?;
        if id as usize != expected_id {
            return Err(corrupted("field_dict: 字段编号不连续"));
        }
        let kind = FieldKind::from_u8(cursor.u8()?)?;
        let name = read_utf8(&mut cursor, "field_dict")?;
        fields.push(FieldDef { id, kind, name });
    }
    if !cursor.is_empty() {
        return Err(corrupted("field_dict: 尾部有残留字节"));
    }
    Ok(fields)
}

/// 编码 zone map 区(仅数值/时间字段;缺失块补空统计)。
pub(crate) fn encode_zmap(
    zones: &ZoneIndex,
    fields: &[(Arc<str>, FieldKind)],
    block_count: usize,
) -> Vec<u8> {
    let mut out = Vec::new();
    let indexed = fields
        .iter()
        .filter(|(_, kind)| *kind != FieldKind::Str)
        .count();
    put_u32(&mut out, indexed as u32);
    put_u64(&mut out, block_count as u64);
    for (field_id, (name, kind)) in fields.iter().enumerate() {
        if *kind == FieldKind::Str {
            continue;
        }
        put_u32(&mut out, field_id as u32);
        for block in 0..block_count {
            let stat = zones.block_stat(name, block).unwrap_or_default();
            out.extend_from_slice(&stat.min.to_le_bytes());
            out.extend_from_slice(&stat.max.to_le_bytes());
            let mut flags = 0_u8;
            if stat.has_value {
                flags |= 1;
            }
            if stat.has_null {
                flags |= 2;
            }
            out.push(flags);
        }
    }
    out
}

/// 编码 `ttl_map`(每块 `min(expires_at)`;无 TTL 行记 `i64::MAX` = +∞)。
///
/// 紧接 zone map 数据之后存放(计入 `zmap_len`)。块内 `min > now` 是"整块记录
/// 均未过期"的充分条件(无 TTL 行视为 +∞),查询期据此免逐行 TTL 判定。
pub(crate) fn encode_ttl_map(min_expires: &[i64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(min_expires.len() * 8);
    for value in min_expires {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// 解码 `ttl_map`(须给定与编码一致的字段字典与块数)。
///
/// # Errors
/// zone map 区结构不符、尾部长度不等于 `block_count × 8` 时返回
/// [`MnemeError::Corrupted`]。
pub(crate) fn decode_ttl_map(
    bytes: &[u8],
    fields: &[FieldDef],
    block_count: usize,
) -> Result<Vec<i64>> {
    let mut cursor = Cursor::new(bytes, "msec zmap");
    let field_count = cursor.u32()? as usize;
    let expected_fields = fields
        .iter()
        .filter(|field| field.kind != FieldKind::Str)
        .count();
    if field_count != expected_fields {
        return Err(corrupted("zmap: 字段数与 field_dict 不符"));
    }
    let stored_blocks = cursor.u64()? as usize;
    if stored_blocks != block_count {
        return Err(corrupted("zmap: 块数与段行数不符"));
    }
    // 跳过 zone map 数据区(field_id + 每块 17 B)。
    for _ in 0..field_count {
        let _ = cursor.u32()?;
        for _ in 0..block_count {
            let _ = cursor.take(17)?;
        }
    }
    let tail = cursor.take(cursor.remaining())?;
    if tail.len() != block_count * 8 {
        return Err(corrupted("zmap: ttl_map 尾部长度不符"));
    }
    Ok(tail
        .chunks_exact(8)
        .map(|chunk| i64::from_le_bytes(chunk.try_into().unwrap_or([0; 8])))
        .collect())
}

/// 校验 zone map 区结构:字段数/块数与字段字典及段行数一致,统计布局完整。
pub(crate) fn validate_zmap(bytes: &[u8], fields: &[FieldDef], block_count: usize) -> Result<()> {
    let expected_fields = fields
        .iter()
        .filter(|field| field.kind != FieldKind::Str)
        .count();
    let mut cursor = Cursor::new(bytes, "msec zmap");
    let field_count = cursor.u32()? as usize;
    if field_count != expected_fields {
        return Err(corrupted("zmap: 字段数与 field_dict 不符"));
    }
    let stored_blocks = cursor.u64()? as usize;
    if stored_blocks != block_count {
        return Err(corrupted("zmap: 块数与段行数不符"));
    }
    for _ in 0..field_count {
        let field_id = cursor.u32()? as usize;
        if field_id >= fields.len() || fields[field_id].kind == FieldKind::Str {
            return Err(corrupted("zmap: 字段编号非法"));
        }
        for _ in 0..block_count {
            let min = f64::from_le_bytes(
                cursor
                    .take(8)?
                    .try_into()
                    .map_err(|_| corrupted("zmap: 区间字节不足"))?,
            );
            let max = f64::from_le_bytes(
                cursor
                    .take(8)?
                    .try_into()
                    .map_err(|_| corrupted("zmap: 区间字节不足"))?,
            );
            let flags = cursor.u8()?;
            if flags & !0b11 != 0 {
                return Err(corrupted("zmap: 统计标志位非法"));
            }
            // ±∞ 是合法的"区间未知"编码(超 `f64` 精确范围的值放弃剪枝);
            // NaN 与 min > max 仍然拒绝。
            if min.is_nan() || max.is_nan() || min > max {
                return Err(corrupted("zmap: 区间非法"));
            }
        }
    }
    let tail = cursor.remaining();
    // 尾部必须恰为 `block_count × 8` 字节的 min(expires_at)。
    if tail != block_count * 8 {
        return Err(corrupted("zmap: ttl_map 尾部长度不符"));
    }
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::super::inverted::decode_inverted;
    use super::*;
    use crate::core::meta::json;
    use crate::core::types::{Key, NsId, RowId, SeqNo};
    use crate::memory::table::SlotData;
    use proptest::prelude::*;

    fn slot_data(index: usize, meta: crate::core::meta::Meta) -> SlotData {
        SlotData {
            rowid: RowId::new(index as u64),
            ns_id: NsId::new(1),
            ns_path: Arc::from("n"),
            seqno: SeqNo::new(index as u64 + 1),
            key: Some(Key::new(format!("k{index}"))),
            vector: Arc::from(vec![0.0_f32].into_boxed_slice()),
            norm_sq: 0.0,
            text: None,
            text_hash: None,
            meta,
            created_at: 1_000,
            expires_at: None,
            importance: 0.5,
            confidence: 1.0,
            valid_from: 1_000,
            valid_to: None,
            provenance: None,
            tx_ms: 1_000,
            deleted: false,
        }
    }

    /// FC-PERSIST-POST-008(字段字典往返)
    #[test]
    fn field_dict_roundtrip() {
        let fields = vec![
            (Arc::from("created_at"), FieldKind::Ts),
            (Arc::from("rank"), FieldKind::Num),
            (Arc::from("key"), FieldKind::Str),
        ];
        let bytes = encode_field_dict(&fields);
        let decoded = decode_field_dict(&bytes).expect("decode");
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[1].name.as_ref(), "rank");
        assert_eq!(decoded[1].kind, FieldKind::Num);
        assert_eq!(decoded[2].kind, FieldKind::Str);
    }

    /// FC-PERSIST-POST-008(zone map 往返与结构校验)
    #[test]
    fn zmap_roundtrip_and_validation() {
        let fields = vec![
            (Arc::from("created_at"), FieldKind::Ts),
            (Arc::from("rank"), FieldKind::Num),
            (Arc::from("key"), FieldKind::Str),
        ];
        let mut zones = ZoneIndex::new(16);
        zones.observe(0, &slot_data(0, json!({"rank": 7})));
        zones.observe(1, &slot_data(1, json!({"rank": 3})));
        let mut bytes = encode_zmap(&zones, &fields, 1);
        bytes.extend_from_slice(&encode_ttl_map(&[i64::MAX]));
        let defs = decode_field_dict(&encode_field_dict(&fields)).expect("defs");
        validate_zmap(&bytes, &defs, 1).expect("valid");
        // 截断与块数不符必须被检出。
        assert!(validate_zmap(&bytes[..bytes.len() - 1], &defs, 1).is_err());
        assert!(validate_zmap(&bytes, &defs, 2).is_err());
    }

    /// FC-LIFE-CPLX-001(`ttl_map` 往返;尾部长度必须恰为 `block_count × 8`)
    #[test]
    fn ttl_map_roundtrip_and_invalid_tail() {
        let fields = vec![
            (Arc::from("created_at"), FieldKind::Ts),
            (Arc::from("key"), FieldKind::Str),
        ];
        let zones = ZoneIndex::new(16);
        let mut with_ttl = encode_zmap(&zones, &fields, 2);
        let defs = decode_field_dict(&encode_field_dict(&fields)).expect("defs");
        with_ttl.extend_from_slice(&encode_ttl_map(&[1_700_000_000_000, i64::MAX]));
        let decoded = decode_ttl_map(&with_ttl, &defs, 2).expect("decode");
        assert_eq!(decoded, vec![1_700_000_000_000, i64::MAX]);
        validate_zmap(&with_ttl, &defs, 2).expect("valid");

        // 缺尾 / 尾长不符 → Corrupted。
        let missing = encode_zmap(&zones, &fields, 2);
        assert!(matches!(
            decode_ttl_map(&missing, &defs, 2),
            Err(MnemeError::Corrupted { .. })
        ));
        assert!(validate_zmap(&missing, &defs, 2).is_err());
        let mut bad = with_ttl.clone();
        bad.push(0);
        assert!(matches!(
            decode_ttl_map(&bad, &defs, 2),
            Err(MnemeError::Corrupted { .. })
        ));
        assert!(validate_zmap(&bad, &defs, 2).is_err());
    }

    /// FC-PERSIST-POST-008(bloom 往返;否定语义保持)
    #[test]
    fn bloom_roundtrip() {
        let mut bloom = BloomSet::new(64, 0.01);
        bloom.insert("alpha");
        bloom.insert("beta");
        let bytes = encode_bloom(&bloom, 7);
        let decoded = decode_bloom(&bytes).expect("decode");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, 7);
        assert!(decoded[0].1.maybe_contains("alpha"));
        assert!(decoded[0].1.maybe_contains("beta"));
        assert!(!decoded[0].1.maybe_contains("definitely-absent"));
    }

    /// FC-PERSIST-ERR-010(畸形区结构 → Corrupted,不 panic)
    #[test]
    fn malformed_regions_are_rejected() {
        assert!(decode_field_dict(&[0xFF, 0xFF, 0xFF, 0xFF]).is_err());
        assert!(decode_bloom(&[]).is_err());
        assert!(decode_inverted(&[0xFF; 4], &[]).is_err());
        // 字段类别未知。
        let mut dict = Vec::new();
        put_u32(&mut dict, 1);
        put_u16(&mut dict, 0);
        dict.push(9);
        put_bytes_u32(&mut dict, b"x");
        assert!(decode_field_dict(&dict).is_err());

        // 字段字典尾部残留:合法条目后多 1 字节必须拒绝。
        let mut dict = encode_field_dict(&[(Arc::from("x"), FieldKind::Num)]);
        assert!(decode_field_dict(&dict).is_ok());
        dict.push(0);
        assert!(decode_field_dict(&dict).is_err());

        // zone map 尾部残留:合法区字节后多 1 字节必须拒绝。
        let fields = vec![(Arc::from("created_at"), FieldKind::Ts)];
        let defs = decode_field_dict(&encode_field_dict(&fields)).expect("defs");
        let mut zmap = encode_zmap(&ZoneIndex::new(16), &fields, 1);
        zmap.extend_from_slice(&encode_ttl_map(&[i64::MAX]));
        assert!(validate_zmap(&zmap, &defs, 1).is_ok());
        zmap.push(0);
        assert!(validate_zmap(&zmap, &defs, 1).is_err());

        // bloom:零位长 / 非 64 倍数 / 哈希位置数越界([1,64] 之外)必须拒绝。
        let bloom_bytes = |bit_len: u32, k: u32| {
            let mut bytes = Vec::new();
            put_u32(&mut bytes, 1);
            put_u16(&mut bytes, 0);
            put_u32(&mut bytes, bit_len);
            put_u32(&mut bytes, k);
            for _ in 0..bit_len.div_ceil(64) {
                put_u64(&mut bytes, 0);
            }
            bytes
        };
        assert!(decode_bloom(&bloom_bytes(0, 7)).is_err());
        assert!(decode_bloom(&bloom_bytes(65, 7)).is_err());
        assert!(decode_bloom(&bloom_bytes(64, 0)).is_err());
        assert!(decode_bloom(&bloom_bytes(64, 65)).is_err());
        // 合法下界对照:bit_len=64、k=64 必须可解码。
        assert!(decode_bloom(&bloom_bytes(64, 64)).is_ok());
    }

    /// FC-PERSIST-ERR-010(任意字节不 panic)
    #[test]
    fn decode_regions_never_panics_on_arbitrary_bytes() {
        proptest!(|(bytes in prop::collection::vec(any::<u8>(), 0..256))| {
            // 只证不 panic:返回 Ok/Err 均允许,不对结构合法性作断言。
            let _ = decode_field_dict(&bytes);
            let _ = decode_bloom(&bytes);
            let _ = validate_zmap(&bytes, &[], 0);
        });
    }
}
