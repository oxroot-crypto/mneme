//! zone map / bloom 块级下推(设计 06 §2)。
//!
//! 对支持下推的叶子条件计算"可能命中"的块位图:`1` = 该块内可能存在匹配行。
//! 只剪除**必然不命中**的块——`Not`(三值语义)与无摘要可用的条件保持全 1,
//! 构成安全超集;最终命中由行级三值求值裁决,故下推绝不改变过滤语义。

use crate::core::bitset::BitSet;
use crate::memory::analysis::{MAX_EXACT_INT, ZONE_BLOCK_ROWS, ZoneKind};
use crate::memory::pred::{CmpOp, Expr, Val};
use crate::memory::table::ReaderView;

/// 下推求值上下文:候选视图与 zone map 覆盖的块数。
struct MaskCtx<'a> {
    view: &'a ReaderView,
    blocks: usize,
}

/// 计算表达式"可能命中"的块位图。
pub(crate) fn block_mask(expr: &Expr, view: &ReaderView) -> BitSet {
    let ctx = MaskCtx {
        view,
        blocks: block_count(view),
    };
    eval(expr, &ctx)
}

/// zone map 覆盖的块数(空视图为 0)。
pub(crate) fn block_count(view: &ReaderView) -> usize {
    view.slots.len().div_ceil(ZONE_BLOCK_ROWS)
}

/// 全 1 位图(`blocks` 块全部可能命中)。
pub(crate) fn full_mask(blocks: usize) -> BitSet {
    let mut bits = BitSet::default();
    for block in 0..blocks {
        bits.set(block);
    }
    bits
}

/// 递归求值:每个分支返回"可能命中块"的超集。
fn eval(expr: &Expr, ctx: &MaskCtx<'_>) -> BitSet {
    match expr {
        Expr::Always => full_mask(ctx.blocks),
        Expr::Never => BitSet::default(),
        Expr::And(parts) => {
            let mut mask = full_mask(ctx.blocks);
            for part in parts {
                mask.intersect_with(&eval(part, ctx));
                if mask.is_empty() {
                    break;
                }
            }
            mask
        }
        Expr::Or(parts) => {
            let mut mask = BitSet::default();
            for part in parts {
                mask.union_with(&eval(part, ctx));
            }
            mask
        }
        // 三值语义:`Not(e)` 命中要求 `e` 确定为 False,块级无法证明 → 不下推。
        Expr::Not(_) => full_mask(ctx.blocks),
        Expr::Cmp { op, field, val } => cmp_mask(*op, field, val, ctx),
        Expr::In(field, vals) => in_mask(field, vals, ctx),
        Expr::Exists(field) => exists_mask(field, ctx),
        Expr::IsNull(field) => is_null_mask(field, ctx),
        // 子串/前缀/后缀/通配无块级摘要可用:保持全 1。
        Expr::Contains(..) | Expr::StartsWith(..) | Expr::EndsWith(..) | Expr::Glob(..) => {
            full_mask(ctx.blocks)
        }
    }
}

/// 比较条件的块位图;`key` 等值先用 bloom 预筛。
///
/// 只有能证明"块内所有值都不满足"时才剪枝:字段类别不匹配、查询值超出
/// `f64` 精确范围等无法可靠判断的情形一律返回全 1,由行级三值求值裁决。
fn cmp_mask(op: CmpOp, field: &str, val: &Val, ctx: &MaskCtx<'_>) -> BitSet {
    if op == CmpOp::Eq && field == "key" {
        return match val {
            Val::Str(text) if !ctx.view.key_bloom.maybe_contains(text) => BitSet::default(),
            _ => full_mask(ctx.blocks),
        };
    }
    let Some(kind) = ctx.view.zones.kind_of(field) else {
        return full_mask(ctx.blocks);
    };
    let Some(value) = numeric_value(kind, val) else {
        return full_mask(ctx.blocks);
    };
    let mut mask = BitSet::default();
    for block in 0..ctx.blocks {
        let Some(stat) = ctx.view.zones.block_stat(field, block) else {
            continue;
        };
        if stat.has_value && possible(op, stat.min, stat.max, value) {
            mask.set(block);
        }
    }
    mask
}

/// `In` 的块位图:各取值的等值块位图并集;`key` 走 bloom 预筛。
fn in_mask(field: &str, vals: &[Val], ctx: &MaskCtx<'_>) -> BitSet {
    if field == "key" {
        let possible = vals.iter().any(|val| match val {
            Val::Str(text) => ctx.view.key_bloom.maybe_contains(text),
            _ => true,
        });
        return if possible {
            full_mask(ctx.blocks)
        } else {
            BitSet::default()
        };
    }
    if ctx.view.zones.kind_of(field).is_none() {
        return full_mask(ctx.blocks);
    }
    let mut mask = BitSet::default();
    for val in vals {
        mask.union_with(&cmp_mask(CmpOp::Eq, field, val, ctx));
    }
    mask
}

/// `Exists`:块内该字段完全未出现时才可剪(含字符串/布尔/null 等非数值值)。
fn exists_mask(field: &str, ctx: &MaskCtx<'_>) -> BitSet {
    if ctx.view.zones.kind_of(field).is_none() {
        return full_mask(ctx.blocks);
    }
    let mut mask = BitSet::default();
    for block in 0..ctx.blocks {
        if ctx
            .view
            .zones
            .block_stat(field, block)
            .is_some_and(|stat| stat.has_any)
        {
            mask.set(block);
        }
    }
    mask
}

/// `IsNull`:仅保留有显式 null 的块。
fn is_null_mask(field: &str, ctx: &MaskCtx<'_>) -> BitSet {
    if ctx.view.zones.kind_of(field).is_none() {
        return full_mask(ctx.blocks);
    }
    let mut mask = BitSet::default();
    for block in 0..ctx.blocks {
        if ctx
            .view
            .zones
            .block_stat(field, block)
            .is_some_and(|stat| stat.has_null)
        {
            mask.set(block);
        }
    }
    mask
}

/// 把 `Val` 按字段类别转成比较用的 `f64`。
///
/// 类型不匹配或整数超出 `f64` 精确范围(>2^53)时返回 `None`——调用方必须
/// 视为"无法判断"并保留全 1,绝不据此剪枝。
fn numeric_value(kind: ZoneKind, val: &Val) -> Option<f64> {
    match (kind, val) {
        (ZoneKind::Num, Val::Int(value)) => exact_int(*value),
        (ZoneKind::Num, Val::Num(value)) if value.is_finite() => Some(*value),
        (ZoneKind::Ts, Val::Ts(value)) => exact_int(*value),
        _ => None,
    }
}

/// 仅当整数可由 `f64` 精确表示时转换,否则返回 `None`。
fn exact_int(value: i64) -> Option<f64> {
    (value.unsigned_abs() <= MAX_EXACT_INT as u64).then_some(value as f64)
}

/// 区间 `[min, max]` 内是否存在满足 `field op value` 的值。
fn possible(op: CmpOp, min: f64, max: f64, value: f64) -> bool {
    match op {
        CmpOp::Eq => value >= min && value <= max,
        // 块内不存在"确定不等于 value"的值时不可能命中。
        CmpOp::Ne => !(min == max && min == value),
        CmpOp::Gt => max > value,
        CmpOp::Ge => max >= value,
        CmpOp::Lt => min < value,
        CmpOp::Le => min <= value,
    }
}
