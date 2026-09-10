//! 过滤 AST 的三值求值(`pred_eval.rs`)。
//!
//! 从 `pred.rs` 拆出的求值器;采用 Kleene 三值逻辑,顶层仅 `True` 命中
//! (设计 03 §5.1、FC-QUERY-ERR-002)。

use std::sync::Arc;

use crate::core::meta::{self, Meta};
use crate::memory::pred::{CmpOp, EvalCtx, Expr, Val};
/// 三值逻辑。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tri {
    True,
    False,
    Unknown,
}

impl Tri {
    fn not(self) -> Tri {
        match self {
            Tri::True => Tri::False,
            Tri::False => Tri::True,
            Tri::Unknown => Tri::Unknown,
        }
    }

    fn and(self, other: Tri) -> Tri {
        match (self, other) {
            (Tri::False, _) | (_, Tri::False) => Tri::False,
            (Tri::True, Tri::True) => Tri::True,
            _ => Tri::Unknown,
        }
    }

    fn or(self, other: Tri) -> Tri {
        match (self, other) {
            (Tri::True, _) | (_, Tri::True) => Tri::True,
            (Tri::False, Tri::False) => Tri::False,
            _ => Tri::Unknown,
        }
    }
}

/// 字符串模式匹配算子(`StartsWith`/`EndsWith`/`Glob` 三个分支同构,合并求值)。
enum StringOp {
    StartsWith,
    EndsWith,
    Glob,
}

impl StringOp {
    fn matches(self, text: &str, needle: &Arc<str>) -> bool {
        match self {
            StringOp::StartsWith => text.starts_with(&**needle),
            StringOp::EndsWith => text.ends_with(&**needle),
            StringOp::Glob => glob_match(needle, text),
        }
    }
}

/// 求值表达式;仅当结果为 `True` 时命中。
pub(crate) fn matches(expr: &Expr, ctx: &EvalCtx<'_>) -> bool {
    eval(expr, ctx) == Tri::True
}

/// 逻辑与的短路求值:任一分量确定 `False` 立即返回。
fn eval_and(parts: &[Expr], ctx: &EvalCtx<'_>) -> Tri {
    let mut acc = Tri::True;
    for part in parts {
        acc = acc.and(eval(part, ctx));
        if acc == Tri::False {
            return Tri::False;
        }
    }
    acc
}

/// 逻辑或的短路求值:任一分量确定 `True` 立即返回。
fn eval_or(parts: &[Expr], ctx: &EvalCtx<'_>) -> Tri {
    let mut acc = Tri::False;
    for part in parts {
        acc = acc.or(eval(part, ctx));
        if acc == Tri::True {
            return Tri::True;
        }
    }
    acc
}

/// `Cmp` 分支:字段缺失或类型不可比时保持 `Unknown`(三值逻辑,绝不按 `False` 处理)。
fn eval_cmp(op: CmpOp, field: &str, val: &Val, ctx: &EvalCtx<'_>) -> Tri {
    match resolve(field, ctx).and_then(|fv| to_val(&fv)) {
        Some(left) => tri_opt(cmp_vals(op, &left, val)),
        None => Tri::Unknown,
    }
}

/// `In` 分支:命中任一元素即 `True`,字段缺失保持 `Unknown`。
fn eval_in(field: &str, vals: &[Val], ctx: &EvalCtx<'_>) -> Tri {
    match resolve(field, ctx).and_then(|fv| to_val(&fv)) {
        Some(left)
            if vals
                .iter()
                .any(|candidate| cmp_vals(CmpOp::Eq, &left, candidate) == Some(true)) =>
        {
            Tri::True
        }
        Some(_) => Tri::False,
        None => Tri::Unknown,
    }
}

/// `Contains` 分支:数组命中任一元素,字符串含子串;其余类型 `Unknown`。
fn eval_contains(field: &str, val: &Val, ctx: &EvalCtx<'_>) -> Tri {
    match resolve(field, ctx) {
        Some(FieldValue::Meta(Meta::Array(items))) => tri_bool(
            items
                .iter()
                .filter_map(json_to_val)
                .any(|item| cmp_vals(CmpOp::Eq, &item, val) == Some(true)),
        ),
        Some(fv) => match to_val(&fv) {
            Some(Val::Str(text)) => match val {
                Val::Str(needle) => tri_bool(text.contains(&**needle)),
                _ => Tri::Unknown,
            },
            _ => Tri::Unknown,
        },
        None => Tri::Unknown,
    }
}

/// `StartsWith`/`EndsWith`/`Glob` 分支:字段须解析为字符串,否则 `Unknown`。
fn eval_string_op(field: &str, needle: &Arc<str>, op: StringOp, ctx: &EvalCtx<'_>) -> Tri {
    match resolve(field, ctx) {
        Some(FieldValue::Reserved(Val::Str(text))) => tri_bool(op.matches(&text, needle)),
        Some(FieldValue::Meta(Meta::String(text))) => tri_bool(op.matches(text, needle)),
        _ => Tri::Unknown,
    }
}

/// AST 分派;各分支委托给小型求值函数,保证单函数体量可控。
fn eval(expr: &Expr, ctx: &EvalCtx<'_>) -> Tri {
    match expr {
        Expr::Always => Tri::True,
        Expr::Never => Tri::False,
        Expr::And(parts) => eval_and(parts, ctx),
        Expr::Or(parts) => eval_or(parts, ctx),
        Expr::Not(inner) => eval(inner, ctx).not(),
        Expr::Exists(field) => tri_bool(resolve(field, ctx).is_some()),
        Expr::IsNull(field) => tri_bool(matches!(
            resolve(field, ctx),
            Some(FieldValue::Meta(Meta::Null))
        )),
        Expr::Cmp { op, field, val } => eval_cmp(*op, field, val, ctx),
        Expr::In(field, vals) => eval_in(field, vals, ctx),
        Expr::Contains(field, val) => eval_contains(field, val, ctx),
        Expr::StartsWith(field, prefix) => eval_string_op(field, prefix, StringOp::StartsWith, ctx),
        Expr::EndsWith(field, suffix) => eval_string_op(field, suffix, StringOp::EndsWith, ctx),
        Expr::Glob(field, pattern) => eval_string_op(field, pattern, StringOp::Glob, ctx),
    }
}

/// 已解析的字段值。
enum FieldValue<'a> {
    /// 引擎保留字段。
    Reserved(Val),
    /// 用户 metadata。
    Meta(&'a Meta),
}

fn resolve<'a>(field: &str, ctx: &EvalCtx<'a>) -> Option<FieldValue<'a>> {
    let slot = ctx.slot;
    match field {
        "rowid" => Some(FieldValue::Reserved(Val::Int(slot.rowid.get() as i64))),
        "key" => slot
            .key
            .as_ref()
            .map(|key| FieldValue::Reserved(Val::Str(Arc::from(key.as_str())))),
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

fn to_val(fv: &FieldValue<'_>) -> Option<Val> {
    match fv {
        FieldValue::Reserved(val) => Some(val.clone()),
        FieldValue::Meta(meta) => json_to_val(meta),
    }
}

fn json_to_val(meta: &Meta) -> Option<Val> {
    match meta {
        Meta::Bool(value) => Some(Val::Bool(*value)),
        Meta::Number(number) => {
            if let Some(int) = number.as_i64() {
                Some(Val::Int(int))
            } else {
                number.as_f64().map(Val::Num)
            }
        }
        Meta::String(text) => Some(Val::Str(Arc::from(text.as_str()))),
        _ => None,
    }
}

fn cmp_vals(op: CmpOp, left: &Val, right: &Val) -> Option<bool> {
    match (left, right) {
        (Val::Int(a), Val::Int(b)) => Some(order(op, a.cmp(b))),
        (Val::Ts(a), Val::Ts(b)) => Some(order(op, a.cmp(b))),
        (Val::Str(a), Val::Str(b)) => Some(order(op, a.cmp(b))),
        (Val::Bool(a), Val::Bool(b)) => Some(order(op, a.cmp(b))),
        (Val::Num(a), Val::Num(b)) => partial_order(op, a.partial_cmp(b)),
        (Val::Int(a), Val::Num(b)) => partial_order(op, (*a as f64).partial_cmp(b)),
        (Val::Num(a), Val::Int(b)) => partial_order(op, a.partial_cmp(&(*b as f64))),
        _ => None,
    }
}

fn order(op: CmpOp, ordering: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering;
    match op {
        CmpOp::Eq => ordering == Ordering::Equal,
        CmpOp::Ne => ordering != Ordering::Equal,
        CmpOp::Gt => ordering == Ordering::Greater,
        CmpOp::Ge => ordering != Ordering::Less,
        CmpOp::Lt => ordering == Ordering::Less,
        CmpOp::Le => ordering != Ordering::Greater,
    }
}

fn partial_order(op: CmpOp, ordering: Option<std::cmp::Ordering>) -> Option<bool> {
    ordering.map(|ordering| order(op, ordering))
}

fn tri_bool(value: bool) -> Tri {
    if value { Tri::True } else { Tri::False }
}

fn tri_opt(value: Option<bool>) -> Tri {
    match value {
        Some(true) => Tri::True,
        Some(false) => Tri::False,
        None => Tri::Unknown,
    }
}

/// 通配符匹配:`*` 匹配任意串,`?` 匹配单个字符。
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    // 经典双指针回溯:O(n·m) 最坏,模式串短时开销可忽略。
    let (mut p, mut t) = (0_usize, 0_usize);
    let mut star: Option<usize> = None;
    let mut star_match = 0_usize;
    while t < txt.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == txt[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            star_match = t;
            p += 1;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            star_match += 1;
            t = star_match;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::table::{AccessStat, SlotData};

    fn ctx_with(meta: Meta) -> (SlotData, AccessStat) {
        let slot = SlotData {
            rowid: crate::core::types::RowId::new(1),
            ns_id: crate::core::types::NsId::new(1),
            ns_path: Arc::from("a"),
            seqno: crate::core::types::SeqNo::new(1),
            key: Some(crate::core::types::Key::new("k")),
            vector: Arc::from(vec![0.0_f32].into_boxed_slice()),
            norm_sq: 0.0,
            text: Some(Arc::from("hello world")),
            text_hash: None,
            meta,
            created_at: 10,
            expires_at: None,
            importance: 0.5,
            confidence: 1.0,
            valid_from: 10,
            valid_to: None,
            provenance: None,
            tx_ms: 10,
            deleted: false,
        };
        (slot, AccessStat::default())
    }

    fn hit(expr: &Expr, slot: &SlotData, access: AccessStat) -> bool {
        matches(
            expr,
            &EvalCtx {
                slot,
                access: Some(access),
            },
        )
    }

    /// FC-QUERY-POST-001
    #[test]
    fn glob_wildcards() {
        assert!(glob_match("a*c", "abbbc"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "abbc"));
        assert!(glob_match("*", ""));
        assert!(glob_match("", ""));
    }

    /// FC-QUERY-ERR-002
    #[test]
    fn missing_field_keeps_not_false() {
        let (slot, access) = ctx_with(crate::core::meta::json!({}));
        let expr = Expr::Not(Box::new(Expr::field("kind").eq("x")));
        assert!(!hit(&expr, &slot, access));
        assert!(!hit(&Expr::Exists("kind".into()), &slot, access));
    }

    /// FC-QUERY-POST-001(保留字段优先于同名 metadata)
    #[test]
    fn reserved_fields_shadow_metadata() {
        let (slot, access) = ctx_with(crate::core::meta::json!({"importance": 0.0}));
        assert!(hit(&Expr::field("importance").gt(0.4), &slot, access));
    }
}
