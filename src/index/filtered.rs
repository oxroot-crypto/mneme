//! 过滤三档搜索(`filtered.rs`,设计 05 §8)。
//!
//! 按选择性 `s = |候选| / |alive|` 自适应:
//! ① 后过滤(`s > post_threshold`)→ 全图遍历 + 放大 `ef`(≤8×)后过滤结果;
//! ② 约束选邻(`brute_threshold < s ≤ post_threshold`)→ 全图遍历(保连通),结果限候选;
//! ③ 候选暴力(`s ≤ brute_threshold` 或候选数 `< max(ef, 1024)`)→ 直接对候选暴力。
//!
//! 档①②为近似(与候选暴力统计等价,`ef→∞` 收敛);档③恒精确。候选数上界用于避免
//! "约束遍历"在候选稀少、图被过滤切断时返回近空结果(设计 05 §8 档③第二条件)。

use crate::core::heap::TopK;
use crate::core::types::{RowId, SlotId};
use crate::memory::index::{IndexSearch, VectorIndex};

use super::hnsw::HnswIndex;

/// 候选暴力档的行数阈值:候选数低于 `max(ef, 1024)` 时直接暴力更划算且更可靠。
const BRUTE_CANDIDATE_CAP: usize = 1024;

/// 执行一次带过滤的索引搜索。
pub(crate) fn search(index: &HnswIndex, params: &IndexSearch<'_>) -> TopK<(RowId, SlotId)> {
    let mut top = TopK::new(params.k, params.metric);
    let count = index.node_count();
    if count == 0 || params.k == 0 {
        return top;
    }

    let alive_count = params.alive.count_ones().max(1);
    let filter_count = params
        .filter
        .map_or(alive_count, |filter| filter.count_ones());
    let selectivity = filter_count as f32 / alive_count as f32;

    let selectable = |node: u32| {
        let slot = index.slot_of(node).get() as usize;
        params.alive.get(slot) && params.filter.is_none_or(|filter| filter.get(slot))
    };

    // 档③:候选暴力(精确)。选择性极低,或候选数低于 `max(ef, 1024)`。
    let brute_cap = params.ef.max(BRUTE_CANDIDATE_CAP);
    if params.filter.is_some()
        && (selectivity <= params.brute_threshold || filter_count < brute_cap)
    {
        for node in 0..count as u32 {
            if !selectable(node) {
                continue;
            }
            let score = index.score_query(params.query, params.query_norm, node);
            top.push(score, (index.rowid(node), index.slot_of(node)));
        }
        return top;
    }

    // 档①②:图搜索。上层贪心下降,第 0 层按选择性放大 `ef`。
    // 两档均在**全图**上穿行(保证连通),结果再按 `selectable` 过滤;区别仅在 `ef`
    // 放大:档①按 `1/s`(上限 8×),档②取 4×。约束遍历(只走过滤内节点)在候选稀疏时
    // 会因图被过滤切断而近乎返回空集,故不采用(仍满足"结果 ⊆ 过滤位图")。
    let top_level = index.max_level() as usize;
    let mut entry = index.entry_node();
    for layer in (1..=top_level).rev() {
        entry = index.greedy_query(params.query, params.query_norm, entry, layer);
    }

    let ef = if selectivity > params.post_threshold {
        let amp = (1.0 / selectivity.max(f32::MIN_POSITIVE)).clamp(1.0, 8.0);
        ((params.ef.max(params.k) as f32) * amp).ceil() as usize
    } else {
        params.ef.max(params.k).saturating_mul(4)
    };

    let always_traverse = |_node: u32| true;
    let candidates = index.search_layer(
        params.query,
        params.query_norm,
        &[entry],
        ef,
        0,
        &always_traverse,
    );
    for (score, node) in candidates {
        if selectable(node) {
            top.push(score, (index.rowid(node), index.slot_of(node)));
        }
    }
    top
}
