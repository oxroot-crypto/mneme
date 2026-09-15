use std::sync::Arc;

use crate::core::meta::Meta;
use crate::memory::pred::{CmpOp, EvalCtx, Expr, Val};

use super::cmp::StringOp;
use super::resolve::{FieldValue, cmp_field, cmp_meta, is_scalar, resolve};
use super::tri::{Tri, tri_bool, tri_opt};

// 单测操作计数:统计 `Contains` 分支的行级求值次数,供计划器重排测试
// 证伪「昂贵谓词未被短路」(线程局部,避免测试间干扰)。
#[cfg(test)]
thread_local! {
    static CONTAINS_EVALS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// 单测探针:`Contains` 分支的行级求值次数(FC-QUERY-POST-009 证伪用)。
#[cfg(test)]
pub(crate) fn contains_evals() -> u64 {
    CONTAINS_EVALS.with(std::cell::Cell::get)
}

/// 单测探针:清零 [`contains_evals`] 计数。
#[cfg(test)]
pub(crate) fn reset_contains_evals() {
    CONTAINS_EVALS.with(|evals| evals.set(0));
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
    match resolve(field, ctx) {
        Some(fv) if is_scalar(&fv) => tri_opt(cmp_field(op, &fv, val)),
        _ => Tri::Unknown,
    }
}

/// `In` 分支:命中任一元素即 `True`,字段缺失或不可标量化保持 `Unknown`。
fn eval_in(field: &str, vals: &[Val], ctx: &EvalCtx<'_>) -> Tri {
    match resolve(field, ctx) {
        Some(fv) if is_scalar(&fv) => tri_bool(
            vals.iter()
                .any(|candidate| cmp_field(CmpOp::Eq, &fv, candidate) == Some(true)),
        ),
        _ => Tri::Unknown,
    }
}

/// `Contains` 分支:数组命中任一元素,字符串含子串;其余类型 `Unknown`。
fn eval_contains(field: &str, val: &Val, ctx: &EvalCtx<'_>) -> Tri {
    #[cfg(test)]
    CONTAINS_EVALS.with(|evals| evals.set(evals.get() + 1));
    match resolve(field, ctx) {
        Some(FieldValue::Meta(Meta::Array(items))) => tri_bool(
            items
                .iter()
                .any(|item| cmp_meta(CmpOp::Eq, item, val) == Some(true)),
        ),
        Some(FieldValue::Reserved(Val::Str(text))) => contains_str(&text, val),
        Some(FieldValue::Text(text)) => contains_str(text, val),
        Some(FieldValue::Meta(Meta::String(text))) => contains_str(text, val),
        _ => Tri::Unknown,
    }
}

/// 字符串包含子串;取值非字符串时保持 `Unknown`(与三值语义一致)。
pub(crate) fn contains_str(text: &str, val: &Val) -> Tri {
    match val {
        Val::Str(needle) => tri_bool(text.contains(&**needle)),
        _ => Tri::Unknown,
    }
}

/// `StartsWith`/`EndsWith`/`Glob` 分支:字段须解析为字符串,否则 `Unknown`。
fn eval_string_op(field: &str, needle: &Arc<str>, op: StringOp, ctx: &EvalCtx<'_>) -> Tri {
    match resolve(field, ctx) {
        Some(FieldValue::Reserved(Val::Str(text))) => tri_bool(op.matches(&text, needle)),
        Some(FieldValue::Text(text)) => tri_bool(op.matches(text, needle)),
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
