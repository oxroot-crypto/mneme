//! 量化副本个规划、抽样召回评估同两阶段精排(设计 08 §落地状态)。

use std::sync::Arc;

use crate::core::bitset::BitSet;
use crate::core::error::{MnemeError, Result};
use crate::core::heap::TopK;
use crate::core::metric::Metric;
use crate::core::options::VectorFormat;
use crate::core::types::SlotId;
use crate::memory::config::Config;
use crate::memory::index::{IndexSearch, QuantCopy, VectorIndex};
use crate::memory::table::WriterState;

use super::types::{QuantDecision, SegmentSlots};

/// 按当前配置为段内向量生成量化副本;i8 逐维统计参数,`f32`/空段返回 `None`。
pub(super) fn plan_quant(config: &Config, vectors: &[&[f32]]) -> Result<Option<QuantCopy>> {
    let format = config.quantization;
    if format == VectorFormat::F32 || vectors.is_empty() {
        return Ok(None);
    }
    let dimension = config.dimension.get() as usize;
    match format {
        VectorFormat::F32 => Ok(None),
        VectorFormat::I8Rescored => Ok(Some(plan_i8_copy(vectors, dimension)?)),
        VectorFormat::F16 => Ok(Some(plan_f16_copy(vectors, dimension)?)),
    }
}

/// i8 量化副本:逐维统计缩放参数,再编码全段行。
fn plan_i8_copy(vectors: &[&[f32]], dimension: usize) -> Result<QuantCopy> {
    let params = crate::quant::scalar_i8::build_params(vectors, dimension)?;
    let table = params.table();
    let mut codes = Vec::with_capacity(vectors.len() * dimension);
    for vector in vectors {
        crate::quant::scalar_i8::encode_row_into(&mut codes, vector, &params);
    }
    let rows = crate::memory::lazy::LazyRows::from_owned(codes, dimension, vectors.len()).ok_or(
        MnemeError::Inconsistent {
            reason: "i8 量化副本行区长度不符",
        },
    )?;
    Ok(QuantCopy {
        format: VectorFormat::I8Rescored,
        params: table,
        rows,
    })
}

/// f16 量化副本(需 feature `quant-f16`;未开启显式 `Unsupported`)。
fn plan_f16_copy(vectors: &[&[f32]], dimension: usize) -> Result<QuantCopy> {
    #[cfg(feature = "quant-f16")]
    {
        let dimension_bytes = dimension * 2;
        let mut codes = Vec::with_capacity(vectors.len() * dimension_bytes);
        for vector in vectors {
            crate::quant::f16::encode_row_into(&mut codes, vector);
        }
        let rows = crate::memory::lazy::LazyRows::from_owned(codes, dimension_bytes, vectors.len())
            .ok_or(MnemeError::Inconsistent {
                reason: "f16 量化副本行区长度不符",
            })?;
        Ok(QuantCopy {
            format: VectorFormat::F16,
            params: Vec::new(),
            rows,
        })
    }
    #[cfg(not(feature = "quant-f16"))]
    {
        let _ = (vectors, dimension);
        Err(MnemeError::Unsupported {
            feature: "quant-f16",
        })
    }
}

/// [`finalize_quant`] 的输入参数。
pub(super) struct FinalizeQuantInput<'a> {
    /// 库配置(召回下限在 `tuning` 内)。
    pub(super) config: &'a Config,
    /// 写状态(f32 原向量精排来源)。
    pub(super) ws: &'a WriterState,
    /// 段内槽位与向量列。
    pub(super) built: &'a SegmentSlots<'a>,
    /// 本次物化的全局槽位。
    pub(super) included: &'a [usize],
    /// 已构建的 f32 图索引(`None` = 未配置索引工厂)。
    pub(super) index: &'a Option<Arc<dyn VectorIndex>>,
    /// 规划出的量化副本(`None` = 无副本)。
    pub(super) planned: Option<QuantCopy>,
}

/// 抽样评估量化召回:一致率达标保留副本,不达标回退 f32(I13)。
pub(super) fn finalize_quant(input: FinalizeQuantInput<'_>) -> Result<QuantDecision> {
    let FinalizeQuantInput {
        config,
        ws,
        built,
        included,
        index,
        planned,
    } = input;
    let Some(copy) = planned else {
        return Ok(QuantDecision {
            copy: None,
            recall_est: None,
            fallback: false,
        });
    };
    let Some(index) = index else {
        return Err(MnemeError::Inconsistent {
            reason: "量化建段缺少索引,无法抽样评估",
        });
    };
    let estimate = estimate_recall(&EstimateRecallInput {
        config,
        ws,
        built,
        included,
        index: index.as_ref(),
    })?;
    if estimate < config.tuning.quant_recall_floor {
        return Ok(QuantDecision {
            copy: None,
            recall_est: None,
            fallback: true,
        });
    }
    Ok(QuantDecision {
        copy: Some(copy),
        recall_est: Some(estimate),
        fallback: false,
    })
}

/// [`estimate_recall`] 的输入参数。
struct EstimateRecallInput<'a> {
    /// 库配置(度量与探查宽度口径)。
    config: &'a Config,
    /// 写状态(f32 原向量精排来源)。
    ws: &'a WriterState,
    /// 段内槽位与向量列。
    built: &'a SegmentSlots<'a>,
    /// 本次物化的全局槽位(建图节点顺序与之对齐)。
    included: &'a [usize],
    /// 段索引(f32 图)。
    index: &'a dyn VectorIndex,
}

/// 建段抽样召回估计:同一 f32 图下,量化粗排 + f32 精排 top-k 与纯 f32 top-k
/// 的平均一致率(等距抽样 `RECALL_SAMPLE_QUERIES` 条自查询)。
fn estimate_recall(input: &EstimateRecallInput<'_>) -> Result<f32> {
    let EstimateRecallInput {
        config,
        included,
        index,
        ..
    } = *input;
    let node_count = index.node_count();
    if node_count == 0 {
        return Ok(1.0);
    }
    let mut alive = BitSet::default();
    for &slot in included {
        alive.set(slot);
    }
    // 注:`alive` 按本次物化的全部槽位置位(含墓碑/死行),与运行期「可见行」
    // 口径不同;参考与候选两侧同口径,故一致率决策不受影响。
    let k = crate::quant::rescore::RECALL_TOP_K.min(node_count);
    let candidate_cap =
        crate::quant::rescore::coarse_candidates(k, config.tuning.rescore_oversample)
            .min(node_count);
    let indices = crate::quant::rescore::sample_indices(node_count);
    let mut total = 0.0_f32;
    for &node in &indices {
        total += recall_agreement_for_node(
            input,
            RecallSample {
                alive: &alive,
                k,
                candidate_cap,
                node,
            },
        );
    }
    Ok(total / indices.len() as f32)
}

/// [`recall_agreement_for_node`] 的单次抽样参数。
struct RecallSample<'a> {
    /// 本次物化槽位的存活位图(含墓碑/死行,与运行期可见行口径不同)。
    alive: &'a BitSet,
    /// 参考 top-k。
    k: usize,
    /// 粗排候选上限。
    candidate_cap: usize,
    /// 自查询节点 id(= 段内下标)。
    node: usize,
}

/// 单条自查询的一致率:同图下量化粗排 + f32 精排 top-k 与纯 f32 top-k 的重合度。
fn recall_agreement_for_node(input: &EstimateRecallInput<'_>, sample: RecallSample<'_>) -> f32 {
    let RecallSample {
        alive,
        k,
        candidate_cap,
        node,
    } = sample;
    let (config, ws, built, index) = (input.config, input.ws, input.built, input.index);
    // 建图节点顺序与 `included` 一致(node id = 段内下标)。
    let query = built.vectors[node];
    let query_norm = if config.metric.needs_norm() {
        crate::memory::search::norm_sq(query)
    } else {
        0.0
    };
    let reference = search_payloads(&SearchPayloadsInput {
        index,
        query,
        query_norm,
        alive,
        k,
        config,
        use_quant: false,
    });
    let coarse = search_payloads(&SearchPayloadsInput {
        index,
        query,
        query_norm,
        alive,
        k: candidate_cap,
        config,
        use_quant: true,
    });
    let candidate = rescore_payloads(&RescorePayloadsInput {
        ws,
        metric: config.metric,
        query,
        query_norm,
        coarse: &coarse,
        k,
    });
    let reference_ids: Vec<u64> = reference.iter().map(|(rowid, _)| rowid.get()).collect();
    let candidate_ids: Vec<u64> = candidate.iter().map(|(rowid, _)| rowid.get()).collect();
    crate::quant::rescore::agreement(&reference_ids, &candidate_ids)
}

/// [`search_payloads`] 的输入参数。
struct SearchPayloadsInput<'a> {
    /// 段索引。
    index: &'a dyn VectorIndex,
    /// 查询向量。
    query: &'a [f32],
    /// 查询向量范数平方。
    query_norm: f32,
    /// 可见版本位图(全局槽位)。
    alive: &'a BitSet,
    /// 返回条数。
    k: usize,
    /// 库配置(探查宽度与过滤阈值口径)。
    config: &'a Config,
    /// 是否使用量化副本做粗排。
    use_quant: bool,
}

/// 在段索引上搜索并返回排序后的 `(RowId, SlotId)` 列表(`use_quant` 控粗排口径)。
fn search_payloads(input: &SearchPayloadsInput<'_>) -> Vec<(crate::core::types::RowId, SlotId)> {
    let SearchPayloadsInput {
        index,
        query,
        query_norm,
        alive,
        k,
        config,
        use_quant,
    } = *input;
    let top = index.search(&IndexSearch {
        query,
        query_norm,
        ef: config.hnsw.ef_search as usize,
        k,
        alive,
        filter: None,
        post_threshold: config.tuning.filter_post_threshold,
        brute_threshold: config.tuning.filter_brute_threshold,
        use_quant,
        bias: None,
    });
    top.into_sorted_vec()
}

/// [`rescore_payloads`] 的输入参数。
struct RescorePayloadsInput<'a> {
    /// 写状态(f32 原向量来源)。
    ws: &'a WriterState,
    /// 距离度量。
    metric: Metric,
    /// 查询向量。
    query: &'a [f32],
    /// 查询向量范数平方。
    query_norm: f32,
    /// 粗排候选(行标识 + 全局槽位)。
    coarse: &'a [(crate::core::types::RowId, SlotId)],
    /// 精排返回条数。
    k: usize,
}

/// 对粗排候选按 f32 原向量精排,取 top-k(两阶段第二阶段)。
fn rescore_payloads(input: &RescorePayloadsInput<'_>) -> Vec<(crate::core::types::RowId, SlotId)> {
    let RescorePayloadsInput {
        ws,
        metric,
        query,
        query_norm,
        coarse,
        k,
    } = *input;
    let mut top = TopK::new(k, metric);
    for &(rowid, slot) in coarse {
        let Some(data) = ws.slots.get(slot.get() as usize) else {
            continue;
        };
        let score = metric.score(query, &data.vector, query_norm, data.norm_sq);
        top.push(score, (rowid, slot));
    }
    top.into_sorted_vec()
}
