use crate::core::meta::{self, Meta};
use crate::memory::pred::{CmpOp, EvalCtx, Val};

use super::tri::{cmp_vals, order};

/// 已解析的字段值(借用优先,免逐行堆分配)。
pub(crate) enum FieldValue<'a> {
    /// 引擎保留字段。
    Reserved(Val),
    /// 用户 metadata。
    Meta(&'a Meta),
    /// 借用的字符串字段(如 `key`),比较期无需转成 owned。
    Text(&'a str),
}

/// 字段值是否可转为比较标量(对象/数组/null/无值保持 `Unknown` 口径)。
pub(crate) fn is_scalar(value: &FieldValue<'_>) -> bool {
    match value {
        FieldValue::Reserved(_) | FieldValue::Text(_) => true,
        FieldValue::Meta(meta) => matches!(meta, Meta::Bool(_) | Meta::Number(_) | Meta::String(_)),
    }
}

/// 字段值与过滤取值的比较;类型不可比返回 `None`(三值 `Unknown`)。
pub(crate) fn cmp_field(op: CmpOp, field: &FieldValue<'_>, val: &Val) -> Option<bool> {
    match field {
        FieldValue::Reserved(reserved) => cmp_vals(op, reserved, val),
        FieldValue::Text(text) => match val {
            Val::Str(expected) => Some(order(op, (*text).cmp(&**expected))),
            _ => None,
        },
        FieldValue::Meta(meta) => cmp_meta(op, meta, val),
    }
}

/// metadata 标量与过滤取值比较(整数优先、`f64` 回退,与 JSON 取值口径一致)。
pub(crate) fn cmp_meta(op: CmpOp, meta: &Meta, val: &Val) -> Option<bool> {
    match meta {
        Meta::Bool(value) => match val {
            Val::Bool(expected) => Some(order(op, value.cmp(expected))),
            _ => None,
        },
        Meta::Number(number) => {
            if let Some(int) = number.as_i64() {
                cmp_vals(op, &Val::Int(int), val)
            } else {
                number
                    .as_f64()
                    .and_then(|num| cmp_vals(op, &Val::Num(num), val))
            }
        }
        Meta::String(text) => match val {
            Val::Str(expected) => Some(order(op, text.as_str().cmp(&**expected))),
            _ => None,
        },
        _ => None,
    }
}

/// 引擎保留字段名全表(与 [`resolve`] 的保留分支一一对应)。
///
/// zone map 侧必须排除这些名字本身:它们的行级值来自 `SlotData`,metadata 里的
/// 同名键永远读不到,把 metadata 统计当剪枝依据会静默漏报(FC-QUERY-POST-005)。
pub(crate) const RESERVED_FIELDS: &[&str] = &[
    "rowid",
    "key",
    "created_at",
    "expires_at",
    "importance",
    "confidence",
    "valid_from",
    "valid_to",
    "last_access",
    "access_count",
    "__ns",
];

/// 判断字段名本身是否为引擎保留字段。
pub(crate) fn is_reserved_field(name: &str) -> bool {
    RESERVED_FIELDS.contains(&name)
}

pub(crate) fn resolve<'a>(field: &str, ctx: &EvalCtx<'a>) -> Option<FieldValue<'a>> {
    let slot = ctx.slot;
    match field {
        "rowid" => Some(FieldValue::Reserved(Val::Int(slot.rowid.get() as i64))),
        "key" => slot.key.as_ref().map(|key| FieldValue::Text(key.as_str())),
        "created_at" => Some(FieldValue::Reserved(Val::Ts(slot.created_at))),
        "expires_at" => slot
            .expires_at
            .map(|value| FieldValue::Reserved(Val::Ts(value))),
        "importance" => Some(FieldValue::Reserved(Val::Num(f64::from(slot.importance)))),
        "confidence" => Some(FieldValue::Reserved(Val::Num(f64::from(slot.confidence)))),
        "valid_from" => Some(FieldValue::Reserved(Val::Ts(slot.valid_from))),
        "valid_to" => slot
            .valid_to
            .map(|value| FieldValue::Reserved(Val::Ts(value))),
        "last_access" => ctx
            .access
            .map(|stat| FieldValue::Reserved(Val::Ts(stat.last_access_ms))),
        "access_count" => Some(FieldValue::Reserved(Val::Int(i64::from(
            ctx.access.map_or(0, |stat| stat.access_count),
        )))),
        "__ns" => Some(FieldValue::Reserved(Val::Str(slot.ns_path.clone()))),
        _ => meta::get_path(&slot.meta, field).map(FieldValue::Meta),
    }
}
