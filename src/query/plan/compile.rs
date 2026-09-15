//! 执行计划的编译入口(设计 06 §2)。
//!
//! 无过滤且目标命名空间内无 TTL 行时,计划只依赖不可变视图,经视图级缓存复用。

use std::sync::Arc;

use crate::core::bitset::BitSet;
use crate::core::types::NsId;
use crate::memory::pred::{self, Expr};
use crate::memory::table::{CachedPlan, ReaderView};
use crate::query::zmap;

use super::filter::{RowFilter, row_matches, row_visible, ttl_unexpired_blocks};

// 单测操作计数:视图级计划缓存的命中/写入(线程局部,避免测试间干扰)。
#[cfg(test)]
thread_local! {
    pub(super) static PLAN_CACHE_HITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    pub(super) static PLAN_CACHE_STORES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// 一次查询的执行计划。
pub(crate) struct Plan {
    /// 过滤后候选槽位(行级残余谓词已求值);缓存命中时与视图缓存零拷贝共享。
    pub(crate) candidates: Arc<Vec<u32>>,
    /// 候选槽位位图(BM25 通道共享);缓存命中时零拷贝共享。
    pub(crate) bits: Arc<BitSet>,
    /// 选择性 = 候选数 / 命名空间活行数(无活行时为 0)。
    pub(crate) selectivity: f32,
}

impl Plan {
    /// 候选槽位切片(BM25 与暴力扫描共用)。
    pub(crate) fn candidates(&self) -> &[u32] {
        self.candidates.as_slice()
    }

    /// 候选位图引用。
    pub(crate) fn bits(&self) -> &BitSet {
        self.bits.as_ref()
    }
}

/// 编译过滤表达式为执行计划。
///
/// 无用户过滤且命名空间内无 `expires_at` 行时,结果只依赖不可变视图,复用
/// [`ReaderView`] 上的视图级缓存(键 `NsId`,命中零拷贝共享);含 TTL 行的
/// 命名空间不写缓存,保证过期随时间实时反映(`FC-QUERY-POST-008`)。
///
/// # Arguments
/// * `view` - 不可变读视图。
/// * `ns_id` - 目标命名空间。
/// * `filter` - 过滤表达式;`None` 表示全量活行。
/// * `now_ms` - 当前时刻(TTL 可见性)。
pub(crate) fn compile(view: &ReaderView, ns_id: NsId, filter: Option<&Expr>, now_ms: i64) -> Plan {
    if filter.is_none()
        && let Some(plan) = cached_plan(view, ns_id)
    {
        return plan;
    }
    let scan = scan_candidates(view, ns_id, filter, now_ms);
    let selectivity = if scan.alive == 0 {
        0.0
    } else {
        scan.candidates.len() as f32 / scan.alive as f32
    };
    let plan = Plan {
        candidates: Arc::new(scan.candidates),
        bits: Arc::new(scan.bits),
        selectivity,
    };
    if filter.is_none() && !scan.has_ttl {
        #[cfg(test)]
        PLAN_CACHE_STORES.with(|stores| stores.set(stores.get() + 1));
        view.store_plan(
            ns_id,
            Arc::new(CachedPlan {
                candidates: Arc::clone(&plan.candidates),
                bits: Arc::clone(&plan.bits),
                selectivity: plan.selectivity,
            }),
        );
    }
    plan
}

/// 无过滤查询命中视图级计划缓存时零拷贝复用(FC-QUERY-POST-008)。
fn cached_plan(view: &ReaderView, ns_id: NsId) -> Option<Plan> {
    let cached = view.cached_plan(ns_id)?;
    #[cfg(test)]
    PLAN_CACHE_HITS.with(|hits| hits.set(hits.get() + 1));
    Some(Plan {
        candidates: Arc::clone(&cached.candidates),
        bits: Arc::clone(&cached.bits),
        selectivity: cached.selectivity,
    })
}

/// 槽位扫描产物:候选槽位、候选位图、活行数与是否含 TTL 行。
struct SlotScan {
    /// 过滤后候选槽位(行级残余谓词已求值)。
    candidates: Vec<u32>,
    /// 候选槽位位图。
    bits: BitSet,
    /// 命名空间内活行数(selectivity 分母)。
    alive: usize,
    /// 命名空间内是否存在 TTL 行(有则不写缓存)。
    has_ttl: bool,
}

/// 逐槽求值可见性与过滤谓词,收集候选与统计信息。
fn scan_candidates(view: &ReaderView, ns_id: NsId, filter: Option<&Expr>, now_ms: i64) -> SlotScan {
    // 合取链按预估选择性重排(FC-QUERY-POST-009):只改变短路求值顺序,
    // 块掩码与行级候选仍与原始 AST 的三值求值逐位全等。
    let reordered = filter.map(pred::reorder_for_eval);
    let filter = reordered.as_ref();
    let mask = filter.map_or_else(
        || zmap::full_mask(zmap::block_count(view)),
        |expr| zmap::block_mask(expr, view),
    );
    // TTL 块级剪枝:块内 `min(expires_at) > now`(或无任何 TTL 值)时整块未过期,
    // 逐行可见性判定可跳过 TTL 比较(FC-LIFE-CPLX-001)。
    let ttl_unexpired = ttl_unexpired_blocks(view, now_ms);
    let ctx = RowFilter {
        view,
        ns_id,
        now_ms,
        ttl_unexpired: &ttl_unexpired,
        mask: &mask,
        filter,
        uses_access: filter.is_some_and(pred::Expr::uses_access),
    };
    collect_slots(view, ns_id, &ctx)
}

/// 逐槽扫描:填充候选/位图,统计活行数并标记是否存在 TTL 行。
fn collect_slots(view: &ReaderView, ns_id: NsId, ctx: &RowFilter<'_>) -> SlotScan {
    let mut scan = SlotScan {
        candidates: Vec::new(),
        bits: BitSet::with_capacity_bits(view.slots.len()),
        alive: 0,
        has_ttl: false,
    };
    for (index, slot) in view.slots.iter().enumerate() {
        // TTL 行(即使当前已过期)一律使缓存失效:过期随时间变化,缓存结果
        // 不可复用(FC-QUERY-POST-008)。
        if slot.ns_id == ns_id && slot.expires_at.is_some() {
            scan.has_ttl = true;
        }
        if !row_visible(ctx, index, slot) {
            continue;
        }
        scan.alive += 1;
        if !row_matches(ctx, index, slot) {
            continue;
        }
        // 槽位下标 ≤ u32::MAX:commit_version 经 `slot_id_for` 拒绝继续增长,
        // 下标越界即违反 FC-MEM-INV-004,故此转换可证明不会失败。
        scan.candidates
            .push(u32::try_from(index).expect("槽位下标必可转入 u32(FC-MEM-INV-004)"));
        scan.bits.set(index);
    }
    scan
}
