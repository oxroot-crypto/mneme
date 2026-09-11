//! 段重建与写状态初始化(`recover/state.rs`)。
//!
//! 把各活跃段的 `vsec`/`msec` 重建成版本链、key 索引与关系边;墓碑以
//! `version_table.doc_offset = TOMBSTONE_DOC_OFFSET` 表示(无记录体),保证
//! "删除永不复活"(I19)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::types::{NsId, SeqNo};
use crate::memory::table::WriterState;
use crate::persist::manifest::Manifest;
use crate::persist::msec::{self, VersionRow};
use crate::persist::vsec;

use super::segment::{apply_relations, apply_versions, load_segment_views};

/// 一个待恢复的段:`(segment_id, vsec 字节, msec 字节, 可选 hidx 字节)`。
pub(crate) struct SegmentBytes {
    /// 段编号。
    pub(crate) segment_id: u32,
    /// 向量段字节。
    pub(crate) vsec: Vec<u8>,
    /// 元数据段字节。
    pub(crate) msec: Vec<u8>,
    /// HNSW 图段字节(`hidx_crc == 0` 时为 `None`)。
    pub(crate) hidx: Option<Vec<u8>>,
}

/// [`load_segments`] 的恢复结果。
pub(crate) struct RecoveredSegments {
    /// 被隔离(跳过)的段 id 列表。
    pub(crate) skipped: Vec<u32>,
    /// 单段场景下"段内槽位 → 全局槽位"的重排映射(用于把 hidx 节点 id 映射回
    /// 恢复后全局 `SlotId`);多段或存在跳过段时为 `None`。
    pub(crate) remap: Option<Vec<u32>>,
}

/// 从零构建写状态,并载入命名空间注册表与 ID 水位。
pub(crate) fn empty_state(manifest: &Manifest) -> WriterState {
    let mut state = WriterState::new();
    for ns in &manifest.namespaces {
        let id = NsId::new(ns.ns_id);
        Arc::make_mut(&mut state.ns_registry).insert(id, Arc::clone(&ns.path));
        Arc::make_mut(&mut state.ns_by_path).insert(Arc::clone(&ns.path), id);
    }
    state.next_ns_id = manifest.next_ns_id;
    state.next_rowid = manifest.next_rowid;
    state.seqno = SeqNo::new(manifest.watermark_seqno);
    state
}

/// 把各段记录重建成写状态(调用前请先用 [`empty_state`] 载入注册表/水位)。
///
/// 返回被隔离(跳过)的段 id 与单段重排映射,供调用方移入 `trash/` 并按需载入 hidx。
///
/// # Errors
/// 段头/记录体损坏且 `fail_fast` 时返回错误;否则损坏段被跳过并计入返回值。
pub(crate) fn load_segments(
    state: &mut WriterState,
    segments: &[SegmentBytes],
    verify_payload: bool,
    fail_fast: bool,
) -> Result<RecoveredSegments> {
    // 恢复期间暂停索引增量维护:段载入完成后由磁盘索引或全量重建接管。
    state.indexing_paused = true;
    // 收集全部版本并按 (rowid, seqno) 全局排序,保证同一 RowId 的版本链有序。
    let mut versions: Vec<(VersionRow, usize)> = Vec::new();
    let mut parsed: Vec<(vsec::VsecView<'_>, msec::MsecView<'_>)> = Vec::new();
    let mut skipped: Vec<u32> = Vec::new();
    for segment in segments {
        let Some((vsec_view, msec_view)) = load_segment_views(segment, verify_payload, fail_fast)?
        else {
            skipped.push(segment.segment_id);
            continue;
        };
        let index = parsed.len();
        for row in msec_view.version_rows()? {
            versions.push((row, index));
        }
        parsed.push((vsec_view, msec_view));
    }
    versions.sort_by_key(|(row, _)| (row.rowid, row.seqno));

    // 单段时构建"段内槽位 → 全局槽位"重排映射(倒排载入与 hidx 均需要)。
    let row_count = parsed
        .first()
        .map_or(0, |(view, _)| view.row_count() as usize);
    let remap = build_remap(&versions, segments, &skipped, row_count)?;

    apply_versions(state, &versions, &parsed)?;
    apply_relations(state, &parsed)?;
    state.pending.clear();
    apply_indexes(state, &parsed, remap.as_deref(), fail_fast)?;
    state.indexing_paused = false;
    Ok(RecoveredSegments { skipped, remap })
}

/// 载入或重建检索加速结构(倒排 / zone map / bloom)。
///
/// 单段且存在重排映射时优先用段内四区装载倒排(免重新分词);结构不合法时
/// `fail_fast` 报错,否则降级为从槽位全量重建——两条路径产生等价的索引。
fn apply_indexes(
    state: &mut WriterState,
    parsed: &[(vsec::VsecView<'_>, msec::MsecView<'_>)],
    remap: Option<&[u32]>,
    fail_fast: bool,
) -> Result<()> {
    if let (Some(remap), [(_, msec_view)]) = (remap, parsed) {
        match load_disk_indexes(state, msec_view, remap) {
            Ok(()) => return Ok(()),
            Err(error) if fail_fast => return Err(error),
            // 索引是查询加速器而非数据来源:损坏时降级全量重建仍然正确。
            Err(_) => {}
        }
    }
    if !parsed.is_empty() {
        state.rebuild_indexes();
    }
    Ok(())
}

/// 由段内四区装载索引:字段字典/zone map 校验,倒排经重排映射,bloom 直接复用。
fn load_disk_indexes(
    state: &mut WriterState,
    msec_view: &msec::MsecView<'_>,
    remap: &[u32],
) -> Result<()> {
    let fields = msec::decode_field_dict(msec_view.field_dict_bytes())?;
    let block_count = state
        .slots
        .len()
        .div_ceil(crate::memory::analysis::ZONE_BLOCK_ROWS);
    msec::validate_zmap(msec_view.zmap_bytes(), &fields, block_count)?;
    let str_ids: Vec<u16> = fields
        .iter()
        .filter(|field| field.kind == msec::FieldKind::Str)
        .map(|field| field.id)
        .collect();
    let blooms = msec::decode_bloom(msec_view.bloom_bytes())?;
    let Some(key_bloom) = blooms
        .into_iter()
        .find_map(|(id, bloom)| str_ids.contains(&id).then_some(bloom))
    else {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "msec: bloom 区缺少 key 字段".to_string(),
        });
    };
    let inv = if msec_view.inverted_bytes().is_empty() {
        crate::memory::analysis::InvertedIndex::default()
    } else {
        msec::decode_inverted(msec_view.inverted_bytes(), remap)?
    };
    state.load_disk_indexes(inv, key_bloom);
    Ok(())
}

/// 构建单段的"段内槽位 → 全局槽位"重排映射。
///
/// 第 k 个被应用的版本落入全局槽位 k(槽位从空开始),故
/// `remap[段内槽位] = 该版本在 `(rowid, seqno)` 有序链中的位置`。
/// 多段或存在跳过段时返回 `None`(调用方降级重建索引)。
///
/// # Errors
/// 版本槽位越界/重复,或存在未被版本行引用的段内槽位(vsec/msec 行数不一致)时
/// 返回 [`MnemeError::Corrupted`]——继续使用会把未引用槽位静默映射到槽位 0。
fn build_remap(
    versions: &[(VersionRow, usize)],
    segments: &[SegmentBytes],
    skipped: &[u32],
    row_count: usize,
) -> Result<Option<Vec<u32>>> {
    if segments.len() != 1 || !skipped.is_empty() {
        return Ok(None);
    }
    let mut remap = vec![0_u32; row_count];
    let mut occupied = vec![false; row_count];
    for (position, (row, _)) in versions.iter().enumerate() {
        let index = row.slot_id as usize;
        if index >= remap.len() || occupied[index] {
            return Err(crate::core::error::MnemeError::Corrupted {
                segment: None,
                reason: "recover: 版本槽位越界或重复".to_string(),
            });
        }
        occupied[index] = true;
        remap[index] = position as u32;
    }
    if occupied.iter().any(|&used| !used) {
        return Err(crate::core::error::MnemeError::Corrupted {
            segment: None,
            reason: "recover: 存在未被版本行引用的段内槽位".to_string(),
        });
    }
    Ok(Some(remap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::MnemeError;
    use crate::persist::msec::VersionRow;

    /// 构造一条版本行(仅 `rowid`/`slot_id` 对本模块测试有意义)。
    fn version(rowid: u64, slot_id: u32) -> VersionRow {
        VersionRow {
            rowid,
            seqno: rowid,
            tx_ms: 0,
            slot_id,
            doc_offset: 0,
        }
    }

    /// 带 `hidx` 的空段占位(字节内容不参与重排映射测试)。
    fn segment_with_hidx() -> SegmentBytes {
        SegmentBytes {
            segment_id: 0,
            vsec: Vec::new(),
            msec: Vec::new(),
            hidx: Some(Vec::new()),
        }
    }

    /// FC-PERSIST-ERR-009:映射按"(rowid, seqno) 有序链位置"重排(非恒等);
    /// 无 hidx 同样构建(倒排载入需要);多段或存在跳过段时返回 `None`。
    #[test]
    fn build_remap_maps_slots_in_version_chain_order() {
        let segments = [segment_with_hidx()];
        // 段内槽位 1 的版本排在链首、槽位 0 排在其后 → 映射必须非恒等。
        let versions = [(version(1, 1), 0), (version(2, 0), 0)];
        let remap = build_remap(&versions, &segments, &[], 2)
            .expect("合法布局不得报错")
            .expect("单段必须产生映射");
        assert_eq!(remap, vec![1, 0], "remap[段内槽位] = 有序链位置");

        let without_hidx = [SegmentBytes {
            hidx: None,
            ..segment_with_hidx()
        }];
        assert_eq!(
            build_remap(&versions, &without_hidx, &[], 2).expect("无 hidx 不是错误"),
            Some(vec![1, 0]),
            "无 hidx 也需映射(倒排载入用)"
        );

        let multi = [segment_with_hidx(), segment_with_hidx()];
        assert_eq!(
            build_remap(&versions, &multi, &[], 2).expect("多段返回 None,非错误"),
            None
        );
    }

    /// FC-PERSIST-ERR-009:槽位越界、重复、存在未被版本行引用的槽位
    /// (vsec/msec 行数不一致)→ `Corrupted`,绝不静默映射到槽位 0。
    #[test]
    fn build_remap_rejects_out_of_range_duplicate_or_unreferenced_slots() {
        let segments = [segment_with_hidx()];

        // 槽位越界:slot 2 不在 [0, row_count)。
        let out_of_range = [(version(1, 2), 0)];
        assert!(matches!(
            build_remap(&out_of_range, &segments, &[], 2),
            Err(MnemeError::Corrupted { .. })
        ));

        // 槽位重复:两条版本行同占 slot 0。
        let duplicate = [(version(1, 0), 0), (version(2, 0), 0)];
        assert!(matches!(
            build_remap(&duplicate, &segments, &[], 2),
            Err(MnemeError::Corrupted { .. })
        ));

        // 未被引用:row_count = 2 但只有 slot 0 出现。
        let unreferenced = [(version(1, 0), 0)];
        assert!(matches!(
            build_remap(&unreferenced, &segments, &[], 2),
            Err(MnemeError::Corrupted { .. })
        ));
    }
}
