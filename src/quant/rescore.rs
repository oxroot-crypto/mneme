//! 两阶段检索预算与建段召回抽样(设计 08 §4)。
//!
//! 粗排候选数 = `top_k × rescore_oversample`(默认 4 倍,即设计中的「4k」,
//! 默认值定义在 core 的 `options::index`,本模块只管预算计算);
//! 精排一律基于 f32 原向量重算并按 f32 分重排,绝不复用量化分
//! (I12 / FC-QUANT-INV-015)。

/// 建段召回估计的抽样查询条数(内部常数;离线 1 万查询基准见设计 14 §3.2)。
pub(crate) const RECALL_SAMPLE_QUERIES: usize = 16;

/// 召回估计的 top-k 口径(设计 Recall@10)。
pub(crate) const RECALL_TOP_K: usize = 10;

/// 两阶段粗排候选数:`top_k × oversample`(饱和乘法;非零倍率下限 1)。
///
/// 与候选总数取小由调用方完成。
pub(crate) fn coarse_candidates(top_k: usize, oversample: usize) -> usize {
    top_k.saturating_mul(oversample.max(1))
}

/// 建段抽样的行下标:等距、确定、不重复;`row_count = 0` 时为空。
pub(crate) fn sample_indices(row_count: usize) -> Vec<usize> {
    if row_count == 0 {
        return Vec::new();
    }
    let count = RECALL_SAMPLE_QUERIES.min(row_count);
    (0..count).map(|index| index * row_count / count).collect()
}

/// 两份 top-k `RowId` 集合的一致率(交集 / 参考集大小);参考集为空视为 1.0。
pub(crate) fn agreement(reference: &[u64], candidate: &[u64]) -> f32 {
    if reference.is_empty() {
        return 1.0;
    }
    let hits = candidate.iter().filter(|id| reference.contains(id)).count();
    hits as f32 / reference.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coarse_candidates_scale_with_top_k() {
        assert_eq!(coarse_candidates(10, 4), 40);
        assert_eq!(coarse_candidates(1, 1), 1);
        assert_eq!(coarse_candidates(10, 0), 10, "零倍率下限收敛为 1");
        assert_eq!(coarse_candidates(usize::MAX, 4), usize::MAX, "饱和不溢出");
    }

    #[test]
    fn sample_indices_are_bounded_and_unique() {
        for rows in [0usize, 1, 3, 16, 100, 10_000] {
            let picked = sample_indices(rows);
            assert!(picked.len() <= RECALL_SAMPLE_QUERIES.min(rows));
            assert!(picked.iter().all(|&index| index < rows.max(1)));
            let mut sorted = picked.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), picked.len(), "抽样下标不得重复");
        }
    }

    #[test]
    fn agreement_counts_intersection_over_reference() {
        assert!((agreement(&[1, 2, 3], &[3, 2, 1]) - 1.0).abs() < 1e-6);
        assert!((agreement(&[1, 2, 3], &[4, 5, 6]) - 0.0).abs() < 1e-6);
        assert!((agreement(&[1, 2, 3], &[1, 2]) - 2.0 / 3.0).abs() < 1e-6);
        assert!((agreement(&[], &[1]) - 1.0).abs() < 1e-6);
    }
}
