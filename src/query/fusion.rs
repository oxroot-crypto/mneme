//! 双通道融合(设计 06 §4)。
//!
//! 向量排名与 BM25 排名量纲不同,直接加权无意义,故提供两种融合:
//! [`Fusion::Rrf`](crate::Fusion::Rrf)(默认,只比名次)与
//! [`Fusion::Weighted`](crate::Fusion::Weighted)(结果集内归一化后加权)。
//! 融合结果同分按 `RowId` 升序,保证排序全等性。

use std::collections::HashMap;

use crate::core::heap::TopK;
use crate::core::metric::Metric;
use crate::core::types::{RowId, SlotId};
use crate::memory::Fusion;
use crate::memory::search::Scored;

/// 融合的公共参数(收拢 `top_k` 与向量通道方向,避免参数过多)。
#[derive(Debug, Clone, Copy)]
pub(crate) struct FusionParams {
    /// 融合后返回条数。
    pub(crate) top_k: usize,
    /// 向量通道分数是否为距离(`Euclidean`;聚合前需翻转方向)。
    pub(crate) vector_is_distance: bool,
}

/// 融合两个通道的命中,返回按融合分降序(同分 `RowId` 升序)的 top-k。
///
/// # Arguments
/// * `vector` - 向量通道命中(已按其优度排序;Euclidean 为距离平方、越小越优)。
/// * `text` - BM25 通道命中(已按分数降序)。
/// * `fusion` - 融合策略。
/// * `params` - 融合公共参数(`top_k` 与向量通道方向)。
pub(crate) fn fuse(
    vector: Vec<Scored>,
    text: Vec<Scored>,
    fusion: Fusion,
    params: FusionParams,
) -> Vec<Scored> {
    match fusion {
        Fusion::Rrf { k } => rrf(vector, text, k, params.top_k),
        Fusion::Weighted { alpha } => weighted(vector, text, alpha, params),
    }
}

/// RRF:分数 = Σ `1 / (k + rank)`,名次从 1 起。
fn rrf(vector: Vec<Scored>, text: Vec<Scored>, k: u32, top_k: usize) -> Vec<Scored> {
    let mut fused: HashMap<RowId, (SlotId, f32)> = HashMap::new();
    for channel in [&vector, &text] {
        for (rank, hit) in channel.iter().enumerate() {
            let add = 1.0 / (k as f32 + (rank + 1) as f32);
            fused.entry(hit.rowid).or_insert((hit.slot, 0.0)).1 += add;
        }
    }
    rank_top(fused, top_k)
}

/// 加权融合:两通道各自在**本次结果集内**归一化后按 `alpha` 加权。
fn weighted(
    vector: Vec<Scored>,
    text: Vec<Scored>,
    alpha: f32,
    params: FusionParams,
) -> Vec<Scored> {
    let mut fused: HashMap<RowId, (SlotId, f32)> = HashMap::new();
    for (rowid, (slot, value)) in normalize(&vector, params.vector_is_distance) {
        fused.entry(rowid).or_insert((slot, 0.0)).1 += alpha * value;
    }
    for (rowid, (slot, value)) in normalize(&text, false) {
        fused.entry(rowid).or_insert((slot, 0.0)).1 += (1.0 - alpha) * value;
    }
    rank_top(fused, params.top_k)
}

/// 结果集内 min-max 归一化;`flip` 时先取负把"越小越优"翻成"越大越优"。
///
/// 通道只有一个结果(极差为 0)时归一值取 1,避免除零。
fn normalize(hits: &[Scored], flip: bool) -> HashMap<RowId, (SlotId, f32)> {
    if hits.is_empty() {
        return HashMap::new();
    }
    let oriented: Vec<f32> = hits
        .iter()
        .map(|hit| if flip { -hit.score } else { hit.score })
        .collect();
    let min = oriented.iter().copied().fold(f32::INFINITY, f32::min);
    let max = oriented.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let span = max - min;
    hits.iter()
        .zip(oriented)
        .map(|(hit, value)| {
            // 仅真正的零极差(单点)取 1;极小极差仍按公式缩放,保住分数差异。
            let normalized = if span == 0.0 {
                1.0
            } else {
                (value - min) / span
            };
            (hit.rowid, (hit.slot, normalized))
        })
        .collect()
}

/// 由 `RowId → (SlotId, 分数)` 取 top-k(同分 `RowId` 升序)。
fn rank_top(fused: HashMap<RowId, (SlotId, f32)>, top_k: usize) -> Vec<Scored> {
    let mut top = TopK::new(top_k, Metric::Dot);
    let mut scores: HashMap<(RowId, SlotId), f32> = HashMap::with_capacity(fused.len());
    for (rowid, (slot, score)) in fused {
        scores.insert((rowid, slot), score);
        top.push(score, (rowid, slot));
    }
    top.into_sorted_vec()
        .into_iter()
        .map(|(rowid, slot)| Scored {
            slot,
            rowid,
            score: scores.get(&(rowid, slot)).copied().unwrap_or(0.0),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(rowid: u64, slot: u32, score: f32) -> Scored {
        Scored {
            slot: SlotId::new(slot),
            rowid: RowId::new(rowid),
            score,
        }
    }

    /// FC-QUERY-POST-004(设计 06 §4.1 算例:A/B 几乎平手、单通道冠军居后)
    #[test]
    fn rrf_matches_design_example() {
        let vector = vec![hit(2, 2, 0.85), hit(1, 1, 0.88), hit(0, 0, 0.91)];
        let text = vec![hit(0, 0, 3.1), hit(1, 1, 2.8)];
        let fused = rrf(vector, text, 60, 3);
        let order: Vec<u64> = fused.iter().map(|hit| hit.rowid.get()).collect();
        assert_eq!(order, vec![0, 1, 2]);
        assert!((fused[0].score - (1.0 / 63.0 + 1.0 / 61.0)).abs() < 1e-6);
        assert!((fused[1].score - (1.0 / 62.0 + 1.0 / 62.0)).abs() < 1e-6);
        assert!((fused[2].score - 1.0 / 61.0).abs() < 1e-6);
    }

    /// FC-QUERY-POST-004(Euclidean 距离先取负再归一,不得排序反转)
    #[test]
    fn weighted_flips_distance_channel() {
        // 向量通道为距离:0.1 最近、0.9 最远;文本通道给出相反顺序。
        let vector = vec![hit(0, 0, 0.1), hit(1, 1, 0.9)];
        let text = vec![hit(1, 1, 2.0), hit(0, 0, 1.0)];
        let fused = weighted(
            vector,
            text,
            0.5,
            FusionParams {
                top_k: 2,
                vector_is_distance: true,
            },
        );
        assert_eq!(fused[0].rowid, RowId::new(0), "距离近者向量归一值应为 1");
        let plain = weighted(
            vec![hit(0, 0, 0.1), hit(1, 1, 0.9)],
            vec![hit(1, 1, 2.0), hit(0, 0, 1.0)],
            0.5,
            FusionParams {
                top_k: 2,
                vector_is_distance: false,
            },
        );
        assert_eq!(plain[0].rowid, RowId::new(1), "同样输入不翻转时结论相反");
    }

    /// FC-QUERY-POST-004(单结果通道归一值取 1,无除零)
    #[test]
    fn weighted_single_result_channel_normalizes_to_one() {
        let vector = vec![hit(0, 0, 0.3)];
        let text = vec![hit(1, 1, 5.0), hit(2, 2, 1.0)];
        let fused = weighted(
            vector,
            text,
            0.5,
            FusionParams {
                top_k: 3,
                vector_is_distance: false,
            },
        );
        let by_id: HashMap<u64, f32> = fused
            .iter()
            .map(|hit| (hit.rowid.get(), hit.score))
            .collect();
        assert_eq!(by_id[&0], 0.5, "单点向量通道归一为 1,权重 0.5");
        assert_eq!(by_id[&1], 0.5, "文本最高分归一为 1,权重 0.5");
        assert_eq!(by_id[&2], 0.0, "文本最低分归一为 0");
    }

    /// FC-QUERY-POST-004(极小极差仍按 min-max 公式,不当作单点)
    #[test]
    fn weighted_tiny_span_still_scales() {
        let vector = vec![hit(0, 0, 1.0), hit(1, 1, 1.0 + f32::EPSILON)];
        let fused = weighted(
            vector,
            Vec::new(),
            1.0,
            FusionParams {
                top_k: 2,
                vector_is_distance: false,
            },
        );
        let by_id: HashMap<u64, f32> = fused
            .iter()
            .map(|hit| (hit.rowid.get(), hit.score))
            .collect();
        assert_eq!(by_id[&0], 0.0);
        assert_eq!(by_id[&1], 1.0);
    }

    /// FC-QUERY-POST-004(同分按 RowId 升序)
    #[test]
    fn ties_break_by_rowid() {
        let vector = vec![hit(9, 0, 1.0)];
        let text = vec![hit(3, 1, 1.0)];
        let fused = rrf(vector, text, 60, 2);
        // 两个文档各只在一个通道出现,且都是一等,RRF 分相同 → 按 RowId 升序。
        assert_eq!(fused[0].rowid, RowId::new(3));
        assert_eq!(fused[1].rowid, RowId::new(9));
    }
}
