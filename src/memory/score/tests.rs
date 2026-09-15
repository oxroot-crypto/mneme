use super::*;

use crate::core::types::RowId;
use crate::memory::search::Scored;

#[test]
fn cosine_sim_bounds() {
    assert!((cosine_sim(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    assert!(cosine_sim(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    assert_eq!(cosine_sim(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
}

#[test]
fn clusters_by_threshold() {
    let a = [1.0_f32, 0.0];
    let b = [1.0_f32, 0.01];
    let c = [0.0_f32, 1.0];
    let groups = cluster_by_similarity(&[&a, &b, &c], 0.99);
    assert_eq!(groups.len(), 2);
}

/// FC-SCORE-CPLX-003(操作计数:MMR 每个候选-已选对至多计算一次对级余弦,
/// 总次数 ≤ `k·m − k(k+1)/2`;未缓存的逐轮重算实现会远超该上界)。
#[test]
fn mmr_caches_pairwise_similarity() {
    let db = crate::memory::Mneme::in_memory(2).expect("in_memory");
    let ns = db.namespace("mmr");
    for index in 0..8 {
        ns.insert(crate::memory::Record::new(vec![index as f32, 1.0]))
            .expect("insert");
    }
    let view = db.table.view();
    let candidates: Vec<(Scored, ScoreBreakdown)> = (0..8)
        .map(|index| {
            (
                Scored {
                    slot: crate::core::types::SlotId::new(index),
                    rowid: RowId::new(u64::from(index)),
                    score: index as f32,
                },
                ScoreBreakdown::default(),
            )
        })
        .collect();
    let (k, m) = (4_usize, candidates.len());
    COSINE_PAIRS.with(|count| count.set(0));
    let selected = mmr_select(&view, candidates, 0.7, k);
    assert_eq!(selected.len(), k);
    let calls = COSINE_PAIRS.with(std::cell::Cell::get) as usize;
    let bound = k * m - k * (k + 1) / 2;
    let uncached: usize = (1..=k).map(|t| t * (m - t + 1)).sum();
    assert!(
        calls <= bound,
        "对级余弦计算 {calls} 超过缓存化上界 {bound}(未缓存实现约 {uncached})"
    );
}
