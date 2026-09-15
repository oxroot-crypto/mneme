//! 段 hidx 索引同量化副本载入(索引是查询加速器,损坏时降级暴力)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::VectorFormat;
use crate::core::types::SlotId;
use crate::memory::index::{
    IndexFactory, IndexLoadRequest, IndexNode, QuantCopy, SegmentIndex, SegmentIndexInput,
    VectorIndex,
};
use crate::memory::lazy::{ByteSource, ByteSpan, LazyRows};
use crate::memory::table::WriterState;
use crate::persist::recover;
use crate::persist::vsec;

use super::options::OpenOptions;

/// [`load_hidx_indexes`] 的输入(参数收敛)。
pub(super) struct HidxxLoadInput<'a> {
    /// 各段字节(含可选 hidx)。
    pub(super) segments: &'a [recover::SegmentBytes],
    /// 恢复结果(重排映射与跳过段)。
    pub(super) recovered: &'a recover::RecoveredSegments,
    /// 打开选项(fail-fast 等)。
    pub(super) options: &'a OpenOptions,
    /// 图构建工厂。
    pub(super) factory: &'a Arc<dyn IndexFactory>,
    /// 距离度量。
    pub(super) metric: Metric,
}

/// 载入各段 hidx 并安装为多段索引(索引是优化:损坏时降级暴力,`check()` 报告)。
///
/// 各段相互独立:段数 ≥ 2 时按可用核数并行载入(打开 1M 多段库时 hidx 卸载
/// 与量化副本解析可线性摊薄),结果按段序回收。
pub(super) fn load_hidx_indexes(
    state: &WriterState,
    input: &HidxxLoadInput<'_>,
) -> Result<Arc<Vec<crate::memory::index::SegmentIndex>>> {
    let jobs = collect_hidx_jobs(input);
    let workers = hidx_load_workers(jobs.len());
    let indexes = if workers <= 1 {
        load_hidx_serial(state, input, &jobs)?
    } else {
        load_hidx_parallel(state, input, &jobs, workers)?
    };
    Ok(Arc::new(indexes))
}

/// 收集待处理段(跳过损坏段与缺失重排映射者),保持输入顺序。
fn collect_hidx_jobs<'a>(
    input: &HidxxLoadInput<'a>,
) -> Vec<(&'a recover::SegmentBytes, &'a recover::SegmentRemap)> {
    let mut jobs: Vec<(&recover::SegmentBytes, &recover::SegmentRemap)> = Vec::new();
    for segment in input.segments {
        if input.recovered.skipped.contains(&segment.segment_id) {
            continue;
        }
        let Some(remap) = input
            .recovered
            .remaps
            .iter()
            .find(|remap| remap.segment_id == segment.segment_id)
        else {
            continue;
        };
        jobs.push((segment, remap));
    }
    jobs
}

/// 载入并行度:min(可用核数, 待载入段数);`wasm` 下恒为 1。
fn hidx_load_workers(job_count: usize) -> usize {
    if cfg!(feature = "wasm") {
        1
    } else {
        std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .min(job_count)
    }
}

/// 串行载入各段索引,保持段序。
fn load_hidx_serial(
    state: &WriterState,
    input: &HidxxLoadInput<'_>,
    jobs: &[(&recover::SegmentBytes, &recover::SegmentRemap)],
) -> Result<Vec<crate::memory::index::SegmentIndex>> {
    let mut indexes = Vec::new();
    for &(segment, remap) in jobs {
        if let Some(index) = build_segment_index(state, input, segment, remap)? {
            indexes.push(index);
        }
    }
    Ok(indexes)
}

/// 按段有界并行载入(worker 轮转分派),结果按段序回收。
fn load_hidx_parallel(
    state: &WriterState,
    input: &HidxxLoadInput<'_>,
    jobs: &[(&recover::SegmentBytes, &recover::SegmentRemap)],
    workers: usize,
) -> Result<Vec<crate::memory::index::SegmentIndex>> {
    let pieces = load_hidx_pieces(state, input, jobs, workers);
    let mut indexed: Vec<Option<crate::memory::index::SegmentIndex>> =
        (0..jobs.len()).map(|_| None).collect();
    for piece in pieces {
        for (job_index, index) in piece? {
            indexed[job_index] = Some(index);
        }
    }
    Ok(indexed.into_iter().flatten().collect())
}

/// 按块并行载入各段(每块一个 worker),结果按块序回收。
fn load_hidx_pieces(
    state: &WriterState,
    input: &HidxxLoadInput<'_>,
    jobs: &[(&recover::SegmentBytes, &recover::SegmentRemap)],
    workers: usize,
) -> Vec<Result<Vec<(usize, crate::memory::index::SegmentIndex)>>> {
    let chunk_size = jobs.len().div_ceil(workers);
    std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .chunks(chunk_size)
            .enumerate()
            .map(|(offset, chunk)| {
                scope.spawn(move || {
                    let mut out = Vec::with_capacity(chunk.len());
                    for (index, (segment, remap)) in chunk.iter().enumerate() {
                        if let Some(built) = build_segment_index(state, input, segment, remap)? {
                            out.push((offset * chunk_size + index, built));
                        }
                    }
                    Ok(out)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                // reason: 载入为只读解析,线程 panic 只可能来自实现 bug;显式转内部
                // 不一致错误而非二次 panic(绝不静默少载段)。
                handle.join().unwrap_or(Err(MnemeError::Inconsistent {
                    reason: "hidx 载入线程 panic",
                }))
            })
            .collect()
    })
}

/// 载入单个段的 hidx 索引;无 hidx 或损坏且非 fail-fast 时返回 `None`
/// (索引是查询加速器而非数据来源,`db.check()` 会校验并报告)。
///
/// # Errors
/// 量化副本解析失败,或索引载入失败且 `fail_fast_on_corruption` 为真时
/// 返回结构化错误。
fn build_segment_index(
    state: &WriterState,
    input: &HidxxLoadInput<'_>,
    segment: &recover::SegmentBytes,
    remap: &recover::SegmentRemap,
) -> Result<Option<crate::memory::index::SegmentIndex>> {
    let Some(hidx) = segment.hidx.as_ref() else {
        return Ok(None);
    };
    let quant = load_quant_copy(segment)?;
    let format = quant.as_ref().map_or(VectorFormat::F32, |copy| copy.format);
    let loaded = load_index(LoadIndexInput {
        factory: input.factory,
        hidx,
        slots: SlotRemap {
            state,
            remap: &remap.remap,
        },
        metric: input.metric,
        quant,
    });
    match loaded {
        Ok(index) => {
            let slots: Vec<SlotId> = remap
                .remap
                .iter()
                .map(|&global| SlotId::new(global))
                .collect();
            Ok(Some(SegmentIndex::new(SegmentIndexInput {
                segment_id: segment.segment_id,
                index,
                slots,
                quant: format,
                recall_est: None,
            })))
        }
        Err(error) if input.options.fail_fast_on_corruption => Err(error),
        // reason: hidx 损坏时降级为暴力扫描仍然正确,`db.check()` 会校验 hidx 字节
        // 并报告损坏;节点数不匹配的降级可由 `stats().segments[*].index_nodes == 0`
        // 观测,绝不静默丢数据。
        Err(_) => Ok(None),
    }
}

/// 从段 vsec 还原量化副本(行顺序 = 段内槽位顺序 = hidx 节点顺序)。
///
/// 码流以惰性行区挂段句柄(不拷贝码字节;FC-PERSIST-INV-021),首次粗排时按需切片。
///
/// # Errors
/// vsec 解析失败、行数与索引不一致,或 f16 段在未开 `quant-f16` 的构建上打开时
/// 返回结构化错误(FC-QUANT-ERR-002)。
fn load_quant_copy(segment: &recover::SegmentBytes) -> Result<Option<QuantCopy>> {
    let view = vsec::parse(segment.vsec_bytes()?)?;
    let format = view.quant();
    if format == VectorFormat::F32 {
        return Ok(None);
    }
    crate::quant::ensure_format_supported(format)?;
    let node_count = view.row_count() as usize;
    let corrupt = |reason: &'static str| MnemeError::Corrupted {
        segment: Some(crate::core::types::SegmentId::new(segment.segment_id)),
        reason: reason.to_string(),
    };
    let (offset, stride) = view
        .quant_region()
        .ok_or_else(|| corrupt("vsec: 量化副本区缺失"))?;
    let byte_len = stride
        .checked_mul(node_count)
        .ok_or_else(|| corrupt("vsec: 量化副本区长度溢出"))?;
    let source = Arc::clone(&segment.vsec) as Arc<dyn crate::memory::lazy::ByteSource>;
    let span = crate::memory::lazy::ByteSpan::new(source, offset, byte_len)
        .ok_or_else(|| corrupt("vsec: 量化副本区越界"))?;
    let rows =
        LazyRows::new(span, stride, node_count).ok_or_else(|| corrupt("vsec: 量化副本行区不符"))?;
    Ok(Some(QuantCopy {
        format,
        params: view.quant_params(),
        rows,
    }))
}

/// [`load_index`] 的槽位来源:恢复后的写状态与"段内槽位 → 全局槽位"重排映射。
pub(super) struct SlotRemap<'a> {
    /// 恢复后的写状态(hidx 节点按段内顺序取 `rowid`/向量)。
    pub(super) state: &'a WriterState,
    /// 段内节点 id → 全局槽位。
    pub(super) remap: &'a [u32],
}

/// [`load_index`] 的输入参数。
pub(super) struct LoadIndexInput<'a> {
    /// 图构建/载入工厂。
    pub(super) factory: &'a Arc<dyn IndexFactory>,
    /// hidx 文件句柄。
    pub(super) hidx: &'a Arc<crate::persist::source::ByteFile>,
    /// 恢复后的槽位来源(写状态与重排映射)。
    pub(super) slots: SlotRemap<'a>,
    /// 距离度量。
    pub(super) metric: Metric,
    /// 段级量化副本(`None` = 纯 f32)。
    pub(super) quant: Option<QuantCopy>,
}

/// 由 hidx 字节与恢复出的槽位构建索引。
///
/// # Errors
/// hidx 解析失败(损坏/版本不一致)或重排映射越界时返回结构化错误。
pub(super) fn load_index(input: LoadIndexInput<'_>) -> Result<Arc<dyn VectorIndex>> {
    let LoadIndexInput {
        factory,
        hidx,
        slots,
        metric,
        quant,
    } = input;
    let source = Arc::clone(hidx) as Arc<dyn ByteSource>;
    let span = ByteSpan::new(source, 0, hidx.len()).ok_or_else(|| MnemeError::Corrupted {
        segment: None,
        reason: "hidx: 句柄区间越界".to_string(),
    })?;
    let mut nodes = Vec::with_capacity(slots.remap.len());
    let mut slot_of = Vec::with_capacity(slots.remap.len());
    for &global in slots.remap {
        let slot = slots
            .state
            .slots
            .get(global as usize)
            .ok_or_else(|| MnemeError::Corrupted {
                segment: None,
                reason: "hidx: 重排映射指向不存在的槽位".to_string(),
            })?;
        nodes.push(IndexNode {
            rowid: slot.rowid,
            vector: Arc::clone(&slot.vector),
            norm_sq: slot.norm_sq,
        });
        slot_of.push(SlotId::new(global));
    }
    factory.load(IndexLoadRequest {
        span: &span,
        nodes: &nodes,
        slot_of: &slot_of,
        metric,
        quant,
    })
}
