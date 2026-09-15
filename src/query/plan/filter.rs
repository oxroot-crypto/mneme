//! 行级可见性 / 候选筛选与 TTL 块级剪枝(设计 06 §2)。
//!
//! 由 `super::compile::compile` 逐槽位调用;只依赖不可变读视图,不做计划缓存。

use crate::core::bitset::BitSet;
use crate::core::types::NsId;
use crate::memory::analysis::ZONE_BLOCK_ROWS;
use crate::memory::pred::{self, EvalCtx, Expr};
use crate::memory::table::{ReaderView, SlotData};
use crate::query::zmap;

// 单测操作计数:统计残余谓词的行级求值次数与 TTL 逐行判定次数
// (线程局部,避免测试间干扰)。
#[cfg(test)]
thread_local! {
    pub(super) static ROW_EVALS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(super) static TTL_CHECKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn row_evals() {
    ROW_EVALS.with(|evals| evals.set(evals.get() + 1));
}

#[cfg(test)]
fn ttl_checks(unproven: bool) {
    if unproven {
        TTL_CHECKS.with(|checks| checks.set(checks.get() + 1));
    }
}

/// 行级筛选上下文(zmap 掩码 / TTL 块证明 / 过滤表达式/命名空间/时刻)。
pub(super) struct RowFilter<'a> {
    /// 不可变读视图。
    pub(super) view: &'a ReaderView,
    /// 目标命名空间。
    pub(super) ns_id: NsId,
    /// 当前时刻(TTL 可见性)。
    pub(super) now_ms: i64,
    /// "整块未过期"位图。
    pub(super) ttl_unexpired: &'a BitSet,
    /// zmap 候选块掩码。
    pub(super) mask: &'a BitSet,
    /// 过滤表达式;`None` = 仅可见性。
    pub(super) filter: Option<&'a Expr>,
    /// 过滤表达式是否引用访问统计(决定行级求值是否查访问表)。
    pub(super) uses_access: bool,
}

/// 行级可见性(未回收 / 命名空间匹配 / 未墓碑与逻辑过期;TTL 块证明时跳过逐行比较)。
pub(super) fn row_visible(ctx: &RowFilter<'_>, index: usize, slot: &SlotData) -> bool {
    let ttl_proven = ctx.ttl_unexpired.get(index / ZONE_BLOCK_ROWS);
    #[cfg(test)]
    ttl_checks(!ttl_proven);
    !ctx.view.dead.get(index)
        && slot.ns_id == ctx.ns_id
        && slot.is_live_with_ttl(ctx.now_ms, !ttl_proven)
}

/// 行级候选筛选(zmap 掩码 + 过滤谓词;计数探针随之内聚)。
pub(super) fn row_matches(ctx: &RowFilter<'_>, index: usize, slot: &SlotData) -> bool {
    if !ctx.mask.get(index / ZONE_BLOCK_ROWS) {
        return false;
    }
    let Some(expr) = ctx.filter else {
        return true;
    };
    #[cfg(test)]
    row_evals();
    pred::matches(
        expr,
        &EvalCtx {
            slot,
            access: ctx
                .uses_access
                .then(|| ctx.view.access.get(&slot.rowid).copied())
                .flatten(),
        },
    )
}

/// 计算"整块记录均未过期"的块位图(块级 TTL 剪枝依据)。
///
/// 判定:`zones` 中 `expires_at` 的块最小值 > `now`(无 TTL 行按 +∞ 处理);
/// 该块无任何 TTL 值时同样视为未过期。统计退化(类型冲突/缺失)只会令该块
/// 退回逐行判定,绝不误判整块过期。
pub(super) fn ttl_unexpired_blocks(view: &ReaderView, now_ms: i64) -> BitSet {
    let blocks = zmap::block_count(view);
    // 字段统计一次定位:循环内直接下标访问,免逐块按字段名哈希。
    let expires = view.zones.field_blocks("expires_at");
    let mut unexpired = BitSet::with_capacity_bits(blocks);
    for block in 0..blocks {
        let proven = match expires.and_then(|stats| stats.get(block)) {
            None => true,
            Some(stat) => !stat.has_value || stat.min > now_ms as f64,
        };
        if proven {
            unexpired.set(block);
        }
    }
    unexpired
}
