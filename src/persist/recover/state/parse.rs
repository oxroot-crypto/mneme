//! 逐段解析、版本行收集同损坏段隔离。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::persist::msec::{self, VersionRow};

use super::types::{ParsedSegment, SegmentBytes};
use crate::persist::recover::segment::load_segment_views;

/// [`load_segments`](super::load::load_segments) 的解析中间态:版本行、已解析段视图与跳过段。
pub(super) struct CollectedSegments<'a> {
    /// `(版本行, 所属已解析段下标)`。
    pub(super) versions: Vec<(VersionRow, usize)>,
    /// 已解析段的 vsec/msec 视图与向量文件句柄。
    pub(super) parsed: Vec<ParsedSegment<'a>>,
    /// 已解析段编号。
    pub(super) parsed_ids: Vec<u32>,
    /// 被隔离(跳过)的损坏段编号。
    pub(super) skipped: Vec<u32>,
}

/// 逐段解析视图并收集全部版本行;损坏段按 `fail_fast` 上报或跳过。
///
/// 段间解析只读且互不依赖,**按段并行**(每段产出视图 + 版本行),再按段号串行
/// 合并——合并顺序决定 `fail_fast` 首个错误的确定性,与串行逐段语义等价。
/// 大库冷开时该步是 O(段数 × 段内行数) 的解码成本,并行后从多核兑现。
pub(super) fn collect_versions<'a>(
    segments: &'a [SegmentBytes],
    verify_payload: bool,
    fail_fast: bool,
) -> Result<CollectedSegments<'a>> {
    // 显式按段号升序回放:全量关系段必须排在其覆盖的旧段之后(写路径恒追加新段,
    // 此处排序是对手工修复/MANIFEST 乱序的防御,FC-PERSIST-POST-012)。
    let mut ordered: Vec<&SegmentBytes> = segments.iter().collect();
    ordered.sort_by_key(|segment| segment.segment_id);
    let results = parse_segments_parallel(&ordered, verify_payload, fail_fast)?;

    let mut versions: Vec<(VersionRow, usize)> = Vec::new();
    let mut parsed: Vec<ParsedSegment<'a>> = Vec::new();
    let mut parsed_ids: Vec<u32> = Vec::new();
    let mut skipped: Vec<u32> = Vec::new();
    for (segment, result) in ordered.iter().zip(results) {
        match result? {
            Some((view, rows)) => {
                let index = parsed.len();
                versions.extend(rows.into_iter().map(|row| (row, index)));
                parsed.push(view);
                parsed_ids.push(segment.segment_id);
            }
            None => skipped.push(segment.segment_id),
        }
    }
    Ok(CollectedSegments {
        versions,
        parsed,
        parsed_ids,
        skipped,
    })
}

/// 单段解析结果:视图 + 版本行;`None` = 损坏段被隔离(非 fail-fast)。
type SegmentParse<'a> = Option<(ParsedSegment<'a>, Vec<VersionRow>)>;

/// 按段并行解析(线程数 = min(可用核数, 段数));结果按段序回收。
///
/// # Errors
/// 解析线程 panic 收敛为 `Inconsistent`;`fail_fast` 的单段错误原样保留在
/// 对应槽位,由调用方按段序返回首个错误。
fn parse_segments_parallel<'a>(
    ordered: &[&'a SegmentBytes],
    verify_payload: bool,
    fail_fast: bool,
) -> Result<Vec<Result<SegmentParse<'a>>>> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |count| count.get())
        .min(ordered.len())
        .max(1);
    if threads <= 1 {
        return Ok(ordered
            .iter()
            .map(|segment| parse_one(segment, verify_payload, fail_fast))
            .collect());
    }
    parse_segments_in_parallel(ordered, verify_payload, fail_fast, threads)
}

/// 多线程并行解析:`threads` 个 worker 轮转取段,结果按段序回收到定长槽位。
fn parse_segments_in_parallel<'a>(
    ordered: &[&'a SegmentBytes],
    verify_payload: bool,
    fail_fast: bool,
    threads: usize,
) -> Result<Vec<Result<SegmentParse<'a>>>> {
    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let mut slots: Vec<Option<Result<SegmentParse<'a>>>> =
        (0..ordered.len()).map(|_| None).collect();
    std::thread::scope(|scope| -> Result<()> {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| -> Vec<(usize, Result<SegmentParse<'a>>)> {
                    let mut produced = Vec::new();
                    loop {
                        let index = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if index >= ordered.len() {
                            break;
                        }
                        produced
                            .push((index, parse_one(ordered[index], verify_payload, fail_fast)));
                    }
                    produced
                })
            })
            .collect();
        for handle in handles {
            let produced = handle.join().map_err(|_| MnemeError::Inconsistent {
                reason: "段解析线程 panic",
            })?;
            for (index, parsed) in produced {
                slots[index] = Some(parsed);
            }
        }
        Ok(())
    })?;
    slots
        .into_iter()
        .map(|slot| {
            slot.ok_or(MnemeError::Inconsistent {
                reason: "段解析结果缺失",
            })
        })
        .collect()
}

/// 解析单段:视图 + 版本表(只解码一次,兼作版本表结构校验)+ 关系/delta 区预校验。
fn parse_one<'a>(
    segment: &'a SegmentBytes,
    verify_payload: bool,
    fail_fast: bool,
) -> Result<SegmentParse<'a>> {
    let Some((vsec_view, msec_view)) = load_segment_views(segment, verify_payload, fail_fast)?
    else {
        return Ok(None);
    };
    // 版本表只解码一次:其结果既是结构预校验,也是本次收集的数据(此前
    // `precheck` 会先解码全表再丢弃,大库冷开时是 O(N) 的重复成本)。
    let version_rows = match msec_view.version_rows() {
        Ok(rows) => rows,
        Err(error) => {
            if fail_fast {
                return Err(with_segment(error, segment.segment_id));
            }
            return Ok(None);
        }
    };
    // 其余区级结构预校验:关系区/delta 区畸形在非 fail-fast 下按段隔离,避免
    // 单段区损坏令整库拒启(与 vsec/msec 解析同口径,FC-PERSIST-ERR-006)。
    if let Err(error) = precheck_segment_aux(&msec_view) {
        if fail_fast {
            return Err(with_segment(error, segment.segment_id));
        }
        return Ok(None);
    }
    Ok(Some((
        ParsedSegment {
            vsec_view,
            msec_view,
            vsec_file: Arc::clone(&segment.vsec),
        },
        version_rows,
    )))
}

/// 预校验关系区与 delta 区结构(版本表由 [`parse_one`] 解码时一并校验)。
fn precheck_segment_aux(msec_view: &msec::MsecView<'_>) -> Result<()> {
    crate::persist::edges::parse(msec_view.relations_bytes())?;
    msec::decode_delta(msec_view.delta_bytes())?;
    Ok(())
}

/// 给区级损坏错误补上段号(仅改写 `Corrupted`,其余原样)。
fn with_segment(error: MnemeError, segment_id: u32) -> MnemeError {
    match error {
        MnemeError::Corrupted { reason, .. } => MnemeError::Corrupted {
            segment: Some(crate::core::types::SegmentId::new(segment_id)),
            reason,
        },
        other => other,
    }
}
