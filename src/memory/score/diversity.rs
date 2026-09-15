use crate::core::simd;
use crate::memory::search::Scored;
use crate::memory::table::ReaderView;

use super::cosine::cosine_from_norms;
use super::types::ScoreBreakdown;

/// 以 MMR 贪心选出至多 `k` 条,保证相关且互相不同。
///
/// 维护各候选与已选集的最大余弦 `max_sim`:每选中一条只对剩余候选各算一次对级
/// 余弦并增量取最大,每个候选-已选对至多计算一次,自范数预计算复用
/// (设计 10 §5.1,FC-SCORE-CPLX-003)。
pub(crate) fn mmr_select(
    view: &ReaderView,
    mut candidates: Vec<(Scored, ScoreBreakdown)>,
    lambda: f32,
    k: usize,
) -> Vec<(Scored, ScoreBreakdown)> {
    let lambda = lambda.clamp(0.0, 1.0);
    let mut selected: Vec<(Scored, ScoreBreakdown)> = Vec::with_capacity(k.min(candidates.len()));
    let mut norms: Vec<f32> = candidates
        .iter()
        .map(|candidate| {
            let vector = &view.slots[candidate.0.slot.get() as usize].vector;
            simd::dot(vector, vector)
        })
        .collect();
    let mut max_sim: Vec<f32> = vec![f32::NEG_INFINITY; candidates.len()];
    while selected.len() < k && !candidates.is_empty() {
        let mut best_idx = 0;
        let mut best_score = f32::NEG_INFINITY;
        for (idx, candidate) in candidates.iter().enumerate() {
            let redundancy = if max_sim[idx] == f32::NEG_INFINITY {
                0.0
            } else {
                max_sim[idx]
            };
            let mmr = lambda * candidate.0.score - (1.0 - lambda) * redundancy;
            if mmr > best_score {
                best_score = mmr;
                best_idx = idx;
            }
        }
        let chosen_norm = norms[best_idx];
        let chosen = candidates.remove(best_idx);
        norms.remove(best_idx);
        max_sim.remove(best_idx);
        if !candidates.is_empty() {
            let chosen_vector = &view.slots[chosen.0.slot.get() as usize].vector;
            for (idx, candidate) in candidates.iter().enumerate() {
                let vector = &view.slots[candidate.0.slot.get() as usize].vector;
                let sim =
                    cosine_from_norms(simd::dot(chosen_vector, vector), chosen_norm, norms[idx]);
                max_sim[idx] = max_sim[idx].max(sim);
            }
        }
        selected.push(chosen);
    }
    selected
}

/// 以并查集把候选按「相似度 ≥ threshold」聚成连通分量。
///
/// 返回每个连通分量的成员下标;单元素簇也会返回,由调用方过滤。
///
/// 复杂度(FC-MODEL-CPLX-002):两两余弦为 $O(n^2\cdot d)$(n = 候选数),并查集近似线性;
/// 空间 $O(n)$。候选规模受单库内存与 `ConsolidationPolicy.filter` 约束,预期 n 为
/// 单命名空间活记录量级;渐进劣化须先修订该契约(FC-GLOBAL-CPLX-001)。
pub(crate) fn cluster_by_similarity(vectors: &[&[f32]], threshold: f32) -> Vec<Vec<usize>> {
    let n = vectors.len();
    let mut parent: Vec<usize> = (0..n).collect();
    // 自范数只算一次,两两比较复用(O(n²·d) 主项不变,常量降为 1/3)。
    let norms: Vec<f32> = vectors
        .iter()
        .map(|vector| simd::dot(vector, vector))
        .collect();
    for i in 0..n {
        for j in (i + 1)..n {
            if cosine_from_norms(simd::dot(vectors[i], vectors[j]), norms[i], norms[j]) >= threshold
            {
                union(&mut parent, i, j);
            }
        }
    }
    let mut groups: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    groups.into_values().collect()
}

/// 并查集查找(路径减半)。
fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

/// 合并两个集合(根不同才连接)。
fn union(parent: &mut [usize], i: usize, j: usize) {
    let (ri, rj) = (find(parent, i), find(parent, j));
    if ri != rj {
        parent[ri] = rj;
    }
}
