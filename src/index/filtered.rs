//! 过滤三档搜索(`filtered.rs`,设计 05 §8)。
//!
//! 按选择性 `s = |候选| / |alive|` 自适应:
//! ① 后过滤(`s > post_threshold`)→ 全图遍历 + 放大 `ef`(≤8×)后过滤结果;
//! ② 放大后过滤(`brute_threshold < s ≤ post_threshold` 且候选数 ≥ `max(ef, 1024)`)
//!    → 全图遍历(保连通),结果限候选,`ef` 取 4×;
//! ③ 候选暴力(`s ≤ brute_threshold` 或候选数 < `max(ef, 1024)`)→ 直接对候选暴力。
//!
//! 档①②为近似(与候选暴力统计等价,`ef→∞` 收敛);档③恒精确。候选数上界用于避免
//! 图遍历在候选稀少时返回近空结果(设计 05 §8 档③第二条件)。

use crate::core::heap::TopK;
use crate::core::types::{RowId, SlotId};
use crate::memory::index::{IndexSearch, VectorIndex};

use super::hnsw::{HnswIndex, QueryRef};

/// 候选暴力档的行数阈值:候选数低于 `max(ef, 1024)` 时直接暴力更划算且更可靠。
const BRUTE_CANDIDATE_CAP: usize = 1024;

/// 档①后过滤的 `ef` 放大上限(按 `1/s`,避免选择性极低时探查宽度失控)。
const MAX_EF_AMPLIFICATION: f32 = 8.0;

/// 档②放大后过滤的固定 `ef` 放大倍数。
const AMPLIFIED_TIER_FACTOR: usize = 4;

/// 图搜索档位(档③在此之前已分流)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphTier {
    /// 档①:后过滤(按 `1/s` 放大,上限 8×)。
    Post,
    /// 档②:放大后过滤(固定 4×)。
    Amplified,
}

// 单测档位/ef 记录:最近一次 `search` 命中的档位与第 0 层探查宽度
// (线程局部,避免测试间干扰)。档位判断只此一处,防止"探针与真实分派漂移"。
#[cfg(test)]
thread_local! {
    static LAST_TIER: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static LAST_EF: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_tier(tier: u8) {
    LAST_TIER.with(|last| last.set(tier));
}

#[cfg(test)]
fn record_ef(ef: usize) {
    LAST_EF.with(|last| last.set(ef));
}

#[cfg(test)]
fn last_tier() -> u8 {
    LAST_TIER.with(std::cell::Cell::get)
}

#[cfg(test)]
fn last_ef() -> usize {
    LAST_EF.with(std::cell::Cell::get)
}

/// 执行一次带过滤的索引搜索。
pub(crate) fn search(index: &HnswIndex, params: &IndexSearch<'_>) -> TopK<(RowId, SlotId)> {
    let count = index.node_count();
    if count == 0 || params.k == 0 {
        return TopK::new(params.k, index.metric());
    }

    let alive_count = params.alive.count_ones().max(1);
    let filter_count = params
        .filter
        .map_or(alive_count, |filter| filter.count_ones());
    let selectivity = filter_count as f32 / alive_count as f32;

    // 档③:候选暴力(精确)。选择性极低,或候选数低于 `max(ef, 1024)`。
    let brute_cap = params.ef.max(BRUTE_CANDIDATE_CAP);
    if params.filter.is_some()
        && (selectivity <= params.brute_threshold || filter_count < brute_cap)
    {
        #[cfg(test)]
        record_tier(3);
        return brute_candidates(index, params, count);
    }

    // 档①/②唯一分派点:后过滤 or 放大后过滤。
    let tier = if selectivity > params.post_threshold {
        GraphTier::Post
    } else {
        GraphTier::Amplified
    };
    #[cfg(test)]
    record_tier(if tier == GraphTier::Post { 1 } else { 2 });
    graph_candidates(index, params, selectivity, tier)
}

/// 节点是否可选:全局槽位在 alive 位图内,且(无过滤或)命中过滤位图。
fn is_selectable(index: &HnswIndex, params: &IndexSearch<'_>, node: u32) -> bool {
    let slot = index.slot_of(node).get() as usize;
    params.alive.get(slot) && params.filter.is_none_or(|filter| filter.get(slot))
}

/// 档③:对候选位图内节点做精确暴力扫描,返回 top-k。
fn brute_candidates(
    index: &HnswIndex,
    params: &IndexSearch<'_>,
    count: usize,
) -> TopK<(RowId, SlotId)> {
    let mut top = TopK::new(params.k, index.metric());
    for node in 0..count as u32 {
        if !is_selectable(index, params, node) {
            continue;
        }
        let score = index.score_query(
            QueryRef {
                vector: params.query,
                norm_sq: params.query_norm,
            },
            node,
        );
        top.push(score, (index.rowid(node), index.slot_of(node)));
    }
    top
}

/// 档①/②:全图遍历(保证连通),结果再按候选取舍;区别仅在 `ef` 放大倍数。
fn graph_candidates(
    index: &HnswIndex,
    params: &IndexSearch<'_>,
    selectivity: f32,
    tier: GraphTier,
) -> TopK<(RowId, SlotId)> {
    let mut top = TopK::new(params.k, index.metric());
    let query = QueryRef {
        vector: params.query,
        norm_sq: params.query_norm,
    };
    let top_level = index.max_level() as usize;
    let mut entry = index.entry_node();
    for layer in (1..=top_level).rev() {
        entry = index.greedy_query(query, entry, layer);
    }

    let base = params.ef.max(params.k);
    let ef = match tier {
        GraphTier::Post => {
            let amp = (1.0 / selectivity.max(f32::MIN_POSITIVE)).clamp(1.0, MAX_EF_AMPLIFICATION);
            ((base as f32) * amp).ceil() as usize
        }
        GraphTier::Amplified => base.saturating_mul(AMPLIFIED_TIER_FACTOR),
    };
    #[cfg(test)]
    record_ef(ef);

    for (score, node) in index.search_layer(query, entry, ef, 0) {
        if is_selectable(index, params, node) {
            top.push(score, (index.rowid(node), index.slot_of(node)));
        }
    }
    top
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::bitset::BitSet;
    use crate::core::metric::Metric;
    use crate::core::options::HnswParams;
    use crate::core::types::RowId;
    use crate::memory::index::IndexNode;

    /// 建一个 2048 节点、8 维的确定性小图(低参数,构建快)。
    fn sample_index() -> HnswIndex {
        let nodes: Vec<IndexNode> = (0..2_048_u64)
            .map(|row| {
                let vector: Vec<f32> = (0..8)
                    .map(|col| ((row * 7 + col * 13) % 101) as f32 / 101.0)
                    .collect();
                let norm_sq = crate::core::simd::dot(&vector, &vector);
                IndexNode {
                    rowid: RowId::new(row),
                    vector: Arc::from(vector.into_boxed_slice()),
                    norm_sq,
                }
            })
            .collect();
        HnswIndex::build(
            &nodes,
            HnswParams {
                m: 4,
                m0: 8,
                ef_construction: 16,
                ef_search: 16,
            },
            Metric::Dot,
        )
    }

    /// 构造测试用搜索参数。
    fn params<'a>(
        query: &'a [f32],
        alive: &'a BitSet,
        filter: Option<&'a BitSet>,
        ef: usize,
        post: f32,
        brute: f32,
    ) -> IndexSearch<'a> {
        IndexSearch {
            query,
            query_norm: crate::core::simd::dot(query, query),
            ef,
            k: 10,
            alive,
            filter,
            post_threshold: post,
            brute_threshold: brute,
        }
    }

    /// 每 `stride` 个节点选一个,作为过滤候选位图。
    fn filter_every(stride: usize, count: usize) -> BitSet {
        let mut filter = BitSet::default();
        for node in (0..count).step_by(stride) {
            filter.set(node);
        }
        filter
    }

    /// FC-INDEX-POST-001:档位选择与选择性/候选数上界一致(①/②/③ 均被命中),
    /// 且 `ef` 放大公式被逐值钉死(区分 `1/s` 放大与固定 4×);
    /// 证明"档②有真实覆盖"而非只测了档③的等价性。
    #[test]
    fn tier_selection_matches_selectivity_and_candidate_cap() {
        let index = sample_index();
        let count = index.node_count();
        let mut alive = BitSet::default();
        for node in 0..count {
            alive.set(node);
        }
        let query = vec![0.4_f32; 8];

        // 无过滤 → 档①(selectivity = 1.0 > post);ef = max(64,10) × 1/1 = 64。
        let _ = search(&index, &params(&query, &alive, None, 64, 0.1, 0.001));
        assert_eq!(last_tier(), 1);
        assert_eq!(last_ef(), 64, "档① s=1.0 时不得放大");

        // s=0.5 > post=0.1 → 档①(候选数 1024 仍 ≥ cap);ef = 64 × 2 = 128。
        let half = filter_every(2, count);
        let _ = search(&index, &params(&query, &alive, Some(&half), 64, 0.1, 0.001));
        assert_eq!(last_tier(), 1);
        assert_eq!(last_ef(), 128, "档① 应按 1/s = 2 放大");

        // brute=0.2 < s=0.5 ≤ post=0.9 且候选数 1024 = max(ef=64,1024) → 档②;
        // ef = max(64,10) × 4 = 256(区分于档①的 128)。
        let _ = search(&index, &params(&query, &alive, Some(&half), 64, 0.9, 0.2));
        assert_eq!(last_tier(), 2);
        assert_eq!(last_ef(), 256, "档② 必须固定 4× 放大");

        // k=10 > ef=4 时放大基数取 max(ef,k)=k:ef = 10 × 4 = 40(覆盖 max(ef,k) 分支)。
        let _ = search(&index, &params(&query, &alive, Some(&half), 4, 0.9, 0.2));
        assert_eq!(last_tier(), 2);
        assert_eq!(last_ef(), 40, "放大基数应为 max(ef, k)");

        // s=0.5 ≤ brute=0.5 → 档③(选择性触发;候选数仍 ≥ cap)。
        let _ = search(&index, &params(&query, &alive, Some(&half), 64, 0.9, 0.5));
        assert_eq!(last_tier(), 3);

        // s≈0.05 > brute 但候选数 < max(ef=128,1024) → 档③(候选数触发)。
        let rare = filter_every(20, count);
        assert!(rare.count_ones() < BRUTE_CANDIDATE_CAP);
        let _ = search(
            &index,
            &params(&query, &alive, Some(&rare), 128, 0.9, 0.001),
        );
        assert_eq!(last_tier(), 3);
    }
}
