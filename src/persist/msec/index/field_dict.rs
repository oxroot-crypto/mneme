//! msec 字段字典区(`field_dict`)编解码。

use std::sync::Arc;

use crate::core::error::Result;
use crate::memory::analysis::ZoneKind;
use crate::persist::{Cursor, put_bytes_u32, put_u16, put_u32};

use super::common::{MAX_FIELDS, corrupted, read_utf8};

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
