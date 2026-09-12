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

use super::segment::{apply_delta, apply_relations, apply_versions, load_segment_views};

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

/// 一个已解析段及其"段内槽位 → 全局槽位"重排映射。
pub(crate) struct SegmentRemap {
    /// 段编号。
    pub(crate) segment_id: u32,
    /// 段内槽位 → 全局槽位(`remap[local] = global`)。
    pub(crate) remap: Vec<u32>,
}

/// [`load_segments`] 的恢复结果。
pub(crate) struct RecoveredSegments {
    /// 被隔离(跳过)的段 id 列表。
    pub(crate) skipped: Vec<u32>,
    /// 每个已解析段的重排映射(倒排载入与 hidx 载入共用)。
    pub(crate) remaps: Vec<SegmentRemap>,
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
/// 返回被隔离(跳过)的段 id 与各段重排映射,供调用方按需载入 hidx。
/// 段按编号升序处理(防御 MANIFEST 乱序):版本链全局排序,关系表与 delta
/// 逐段交错回放,保证时序正确。
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
    state.is_indexing_paused = true;
    let collected = collect_versions(segments, verify_payload, fail_fast)?;
    let mut versions = collected.versions;
    let parsed = collected.parsed;
    let parsed_ids = collected.parsed_ids;
    let skipped = collected.skipped;
    versions.sort_by_key(|(row, _)| (row.rowid, row.seqno));
    // 记录被隔离(损坏)的段:compaction 计划必须排除它们,绝不把损坏段当活跃段
    // 合并清除(FC-PERSIST-ERR-006)。
    state.unavailable_segments = Arc::new(
        skipped
            .iter()
            .copied()
            .collect::<std::collections::HashSet<u32>>(),
    );

    // 每段构建"段内槽位 → 全局槽位"重排映射(倒排载入与 hidx 载入均需要)。
    let row_counts: Vec<usize> = parsed
        .iter()
        .map(|(view, _)| view.row_count() as usize)
        .collect();
    let remaps = build_remaps(&versions, &row_counts, &parsed_ids)?;

    apply_versions(state, &versions, &parsed)?;
    backfill_slot_segments(state, &remaps);
    for (_, msec_view) in parsed.iter() {
        apply_relations(state, msec_view)?;
        apply_delta(state, msec_view)?;
    }
    state.pending.clear();
    apply_indexes(state, &parsed, &remaps, fail_fast)?;
    state.is_indexing_paused = false;
    Ok(RecoveredSegments { skipped, remaps })
}

/// [`load_segments`] 的解析中间态:版本行、已解析段视图与跳过段。
struct CollectedSegments<'a> {
    /// `(版本行, 所属已解析段下标)`。
    versions: Vec<(VersionRow, usize)>,
    /// 已解析段的 vsec/msec 视图。
    parsed: Vec<(vsec::VsecView<'a>, msec::MsecView<'a>)>,
    /// 已解析段编号。
    parsed_ids: Vec<u32>,
    /// 被隔离(跳过)的损坏段编号。
    skipped: Vec<u32>,
}

/// 逐段解析视图并收集全部版本行;损坏段按 `fail_fast` 上报或跳过。
fn collect_versions<'a>(
    segments: &'a [SegmentBytes],
    verify_payload: bool,
    fail_fast: bool,
) -> Result<CollectedSegments<'a>> {
    let mut versions: Vec<(VersionRow, usize)> = Vec::new();
    let mut parsed: Vec<(vsec::VsecView<'a>, msec::MsecView<'a>)> = Vec::new();
    let mut parsed_ids: Vec<u32> = Vec::new();
    let mut skipped: Vec<u32> = Vec::new();
    // 显式按段号升序回放:全量关系段必须排在其覆盖的旧段之后(写路径恒追加新段,
    // 此处排序是对手工修复/MANIFEST 乱序的防御,FC-PERSIST-POST-012)。
    let mut ordered: Vec<&SegmentBytes> = segments.iter().collect();
    ordered.sort_by_key(|segment| segment.segment_id);
    for segment in ordered {
        let Some((vsec_view, msec_view)) = load_segment_views(segment, verify_payload, fail_fast)?
        else {
            skipped.push(segment.segment_id);
            continue;
        };
        // 区级结构预校验:版本表/关系区/delta 区畸形在非 fail-fast 下按段隔离,
        // 避免单段区损坏令整库拒启(与 vsec/msec 解析同口径,FC-PERSIST-ERR-006);
        // fail-fast 时补上段号便于定位。
        if let Err(error) = precheck_segment(&msec_view) {
            if fail_fast {
                return Err(with_segment(error, segment.segment_id));
            }
            skipped.push(segment.segment_id);
            continue;
        }
        let index = parsed.len();
        for row in msec_view.version_rows()? {
            versions.push((row, index));
        }
        parsed.push((vsec_view, msec_view));
        parsed_ids.push(segment.segment_id);
    }
    Ok(CollectedSegments {
        versions,
        parsed,
        parsed_ids,
        skipped,
    })
}

/// 预校验区级结构(版本表 / 关系区 / delta 区)。
fn precheck_segment(msec_view: &msec::MsecView<'_>) -> Result<()> {
    msec_view.version_rows()?;
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

/// 回填"槽位 → 所属段"归属。
///
/// 恢复出的槽位都属于其来源段,供增量 flush 与 compaction 辨识已落盘数据;
/// 缺失会让 `unpersisted_slots` 把全部活跃槽位当作未落盘,重开后的 compaction
/// 会以空段替换活跃段集(永久丢数据,FC-PERSIST-POST-012)。
fn backfill_slot_segments(state: &mut WriterState, remaps: &[SegmentRemap]) {
    for remap in remaps {
        let slot_segment = Arc::make_mut(&mut state.slot_segment);
        for &global in &remap.remap {
            if let Some(entry) = slot_segment.get_mut(global as usize) {
                *entry = Some(remap.segment_id);
            }
        }
    }
}

/// 载入或重建检索加速结构(倒排 / zone map / bloom)。
///
/// 单段且存在重排映射时优先用段内四区装载倒排(免重新分词);多段时逐段解码倒排
/// 并按全局槽位合并,bloom/zone map 从槽位重建;结构不合法时 `fail_fast` 报错,
/// 否则降级为从槽位全量重建——两条路径产生等价的索引。
fn apply_indexes(
    state: &mut WriterState,
    parsed: &[(vsec::VsecView<'_>, msec::MsecView<'_>)],
    remaps: &[SegmentRemap],
    fail_fast: bool,
) -> Result<()> {
    if parsed.is_empty() {
        return Ok(());
    }
    if let ([(_, msec_view)], [remap]) = (parsed, remaps) {
        match load_disk_indexes(state, msec_view, &remap.remap) {
            // 新段四区完整:直接复用磁盘索引。
            Ok(true) => return Ok(()),
            // 旧版段未写四区:显式降级全量重建,不是损坏。
            Ok(false) => {}
            Err(error) if fail_fast => return Err(error),
            // 索引是查询加速器而非数据来源:损坏时降级全量重建仍然正确。
            Err(_) => {}
        }
        state.rebuild_indexes();
        return Ok(());
    }

    // 多段:逐段解码倒排(经各自重排映射)并合并;任一结构不一致即整库重建。
    let mut merged = crate::memory::analysis::InvertedIndex::default();
    for ((_, msec_view), remap) in parsed.iter().zip(remaps.iter()) {
        match decode_segment_index(msec_view, &remap.remap) {
            Ok(SegmentIndexLoad::Loaded(inv)) => merged.merge_from(inv),
            Ok(SegmentIndexLoad::LegacyRebuild) => {
                state.rebuild_indexes();
                return Ok(());
            }
            Err(error) if fail_fast => return Err(error),
            // 索引是查询加速器而非数据来源:损坏时降级全量重建仍然正确。
            Err(_) => {
                state.rebuild_indexes();
                return Ok(());
            }
        }
    }
    state.load_merged_indexes(merged, None);
    Ok(())
}

/// 单段索引区装载结果。
enum SegmentIndexLoad {
    /// 四区结构校验通过,倒排已解码。
    Loaded(crate::memory::analysis::InvertedIndex),
    /// 旧格式段(四区全空),调用方应全量重建。
    LegacyRebuild,
}

/// 校验单段四区结构并解码倒排(多段恢复路径)。
///
/// 与单段路径同口径:字段字典/zone map/`ttl_map`/bloom 全部校验;任一畸形返回
/// `Corrupted`(调用方按 `fail_fast` 决定上报或降级重建),旧格式段显式返回
/// [`SegmentIndexLoad::LegacyRebuild`]。
fn decode_segment_index(msec_view: &msec::MsecView<'_>, remap: &[u32]) -> Result<SegmentIndexLoad> {
    if msec_view.field_dict_bytes().is_empty() {
        // 旧版段四区全空 → 走重建;半新半旧属结构不一致,按损坏拒绝。
        let others_empty = msec_view.zmap_bytes().is_empty()
            && msec_view.bloom_bytes().is_empty()
            && msec_view.inverted_bytes().is_empty();
        if others_empty {
            return Ok(SegmentIndexLoad::LegacyRebuild);
        }
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "msec: field_dict 为空但其它索引区非空".to_string(),
        });
    }
    let fields = msec::decode_field_dict(msec_view.field_dict_bytes())?;
    let block_count =
        (msec_view.row_count() as usize).div_ceil(crate::memory::analysis::ZONE_BLOCK_ROWS);
    msec::validate_zmap(msec_view.zmap_bytes(), &fields, block_count)?;
    let _ttl_map = msec::decode_ttl_map(msec_view.zmap_bytes(), &fields, block_count)?;
    msec::decode_bloom(msec_view.bloom_bytes())?;
    let inv = if msec_view.inverted_bytes().is_empty() {
        crate::memory::analysis::InvertedIndex::default()
    } else {
        msec::decode_inverted(msec_view.inverted_bytes(), remap)?
    };
    Ok(SegmentIndexLoad::Loaded(inv))
}

/// 由段内四区装载索引:字段字典/zone map 校验,倒排经重排映射,bloom 直接复用。
///
/// # Returns
/// `Ok(false)` 表示该段为旧格式(未写四区),调用方应走全量重建;
/// `Ok(true)` 表示磁盘索引已装载。
fn load_disk_indexes(
    state: &mut WriterState,
    msec_view: &msec::MsecView<'_>,
    remap: &[u32],
) -> Result<bool> {
    if msec_view.field_dict_bytes().is_empty() {
        // 旧版段四区全空:显式跳过磁盘索引,交由重建路径。
        let others_empty = msec_view.zmap_bytes().is_empty()
            && msec_view.bloom_bytes().is_empty()
            && msec_view.inverted_bytes().is_empty();
        if !others_empty {
            // 半新半旧(字段字典缺失但其它索引区非空)属结构不一致,按损坏拒绝。
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: "msec: field_dict 为空但其它索引区非空".to_string(),
            });
        }
        return Ok(false);
    }
    let fields = msec::decode_field_dict(msec_view.field_dict_bytes())?;
    validate_disk_regions(state, msec_view, &fields)?;
    let key_bloom = decode_key_bloom(msec_view, &fields)?;
    let inv = if msec_view.inverted_bytes().is_empty() {
        crate::memory::analysis::InvertedIndex::default()
    } else {
        msec::decode_inverted(msec_view.inverted_bytes(), remap)?
    };
    state.load_disk_indexes(inv, key_bloom);
    Ok(true)
}

/// 校验段内 zone map / `ttl_map` 结构(块数按恢复后的全局槽位数)。
fn validate_disk_regions(
    state: &WriterState,
    msec_view: &msec::MsecView<'_>,
    fields: &[msec::FieldDef],
) -> Result<()> {
    let block_count = state
        .slots
        .len()
        .div_ceil(crate::memory::analysis::ZONE_BLOCK_ROWS);
    msec::validate_zmap(msec_view.zmap_bytes(), fields, block_count)?;
    // `ttl_map`(块级 TTL 剪枝元数据)在载入期解码校验;运行期 zone map 从槽位
    // 重建,该元数据不直接参与查询(设计 04 §5.2,FC-LIFE-CPLX-001)。
    let _ttl_map = msec::decode_ttl_map(msec_view.zmap_bytes(), fields, block_count)?;
    Ok(())
}

/// 从 bloom 区取 `key` 字段的布隆过滤器;缺失按损坏拒绝。
fn decode_key_bloom(
    msec_view: &msec::MsecView<'_>,
    fields: &[msec::FieldDef],
) -> Result<crate::memory::analysis::BloomSet> {
    let str_ids: Vec<u16> = fields
        .iter()
        .filter(|field| field.kind == msec::FieldKind::Str)
        .map(|field| field.id)
        .collect();
    let blooms = msec::decode_bloom(msec_view.bloom_bytes())?;
    blooms
        .into_iter()
        .find_map(|(id, bloom)| str_ids.contains(&id).then_some(bloom))
        .ok_or_else(|| MnemeError::Corrupted {
            segment: None,
            reason: "msec: bloom 区缺少 key 字段".to_string(),
        })
}

/// 为每个已解析段构建"段内槽位 → 全局槽位"重排映射。
///
/// 第 k 个被应用的版本落入全局槽位 k(槽位从空开始),故
/// `remap[段内槽位] = 该版本在 (rowid, seqno) 有序链中的位置`。多段各自独立映射。
///
/// # Errors
/// 版本槽位越界/重复,或存在未被版本行引用的段内槽位(vsec/msec 行数不一致)时
/// 返回 [`MnemeError::Corrupted`]——继续使用会把未引用槽位静默映射到槽位 0。
fn build_remaps(
    versions: &[(VersionRow, usize)],
    row_counts: &[usize],
    parsed_ids: &[u32],
) -> Result<Vec<SegmentRemap>> {
    let mut remaps: Vec<Vec<u32>> = row_counts.iter().map(|&count| vec![0_u32; count]).collect();
    let mut occupied: Vec<Vec<bool>> = row_counts.iter().map(|&count| vec![false; count]).collect();
    for (position, (row, segment)) in versions.iter().enumerate() {
        let remap = &mut remaps[*segment];
        let index = row.slot_id as usize;
        if index >= remap.len() || occupied[*segment][index] {
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: "recover: 版本槽位越界或重复".to_string(),
            });
        }
        occupied[*segment][index] = true;
        // 槽位总数受 `slot_id_for`(FC-MEM-INV-004)约束在 u32 内,可证明转换安全;
        // 仍用 checked 形式避免静默截断。
        remap[index] = u32::try_from(position).map_err(|_| MnemeError::LimitExceeded {
            field: "remap position",
            limit: u32::MAX as usize,
            got: position,
        })?;
    }
    for used in &occupied {
        if used.iter().any(|&slot| !slot) {
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: "recover: 存在未被版本行引用的段内槽位".to_string(),
            });
        }
    }
    Ok(parsed_ids
        .iter()
        .zip(remaps)
        .map(|(&segment_id, remap)| SegmentRemap { segment_id, remap })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::MnemeError;
    use crate::persist::msec::VersionRow;

    /// 构造只带 relations 区的空 msec 段(无槽位)。
    fn msec_only_segment(relations: &[u8]) -> Vec<u8> {
        msec::encode(&msec::MsecInput {
            slots: &[],
            ns_stats: &[],
            delta: &[],
            relations,
            field_dict: &[],
            zmap: &[],
            bloom: &[],
            inverted: &[],
        })
        .expect("encode")
    }

    /// 构造一个空的合法段字节(用于恢复顺序测试)。
    fn empty_segment_bytes(id: u32) -> SegmentBytes {
        let vsec = vsec::encode(&vsec::VsecInput {
            dimension: 2,
            metric: crate::core::metric::Metric::Cosine,
            created_unix_ms: 0,
            vectors: &[],
            norms: &[],
            dead: &[],
        })
        .expect("vsec");
        SegmentBytes {
            segment_id: id,
            vsec,
            msec: msec_only_segment(&[]),
            hidx: None,
        }
    }

    /// FC-MODEL-POST-007:无 `FLAG_FULL` 的增量段按 upsert 应用(空关系表)
    /// 不得清掉先前段建立的边。
    #[test]
    fn incremental_relations_are_upserted() {
        let edge = crate::persist::edges::EdgeData {
            from: 7,
            to: 9,
            kind: 1,
            weight: 0.5,
            meta: crate::core::meta::Meta::Null,
        };
        let with_edge = crate::persist::edges::encode(&[edge], false, false).expect("encode");
        let empty = crate::persist::edges::encode(&[], false, false).expect("encode");

        let mut state = WriterState::new();
        let seg0 = msec_only_segment(&with_edge);
        let view0 = msec::parse(&seg0).expect("parse seg0");
        apply_relations(&mut state, &view0).expect("apply seg0");
        assert_eq!(
            state
                .out_edges
                .get(&crate::core::types::RowId::new(7))
                .map(Vec::len),
            Some(1)
        );

        // 增量段(空关系表、无 FULL 位):upsert 不得清掉先前个边。
        let seg1 = msec_only_segment(&empty);
        let view1 = msec::parse(&seg1).expect("parse seg1");
        apply_relations(&mut state, &view1).expect("apply seg1");
        assert_eq!(
            state
                .out_edges
                .get(&crate::core::types::RowId::new(7))
                .map(Vec::len),
            Some(1),
            "增量段 upsert 不得清掉先前段个边"
        );
    }

    /// FC-PERSIST-POST-012:恢复按段号升序回放(防御 MANIFEST 乱序/手工修复)。
    #[test]
    fn collect_versions_orders_segments_by_id() {
        let segments = [empty_segment_bytes(1), empty_segment_bytes(0)];
        let collected = collect_versions(&segments, false, false).expect("collect");
        assert_eq!(collected.parsed_ids, vec![0, 1], "段必须按编号升序回放");
    }

    /// 构造旧格式(0x0001)空段:头部合法、四区全部为空。
    fn empty_legacy_msec() -> Vec<u8> {
        let mut bytes = vec![0_u8; crate::persist::msec::HEADER_LEN as usize];
        bytes[0..4].copy_from_slice(b"MSC1");
        bytes[4..6].copy_from_slice(&0x0001_u16.to_le_bytes());
        bytes[6..8].copy_from_slice(&crate::persist::msec::HEADER_LEN.to_le_bytes());
        let crc = crate::persist::crc32(&bytes[0..160]);
        bytes[160..164].copy_from_slice(&crc.to_le_bytes());
        bytes.extend_from_slice(&crate::persist::crc32(&[]).to_le_bytes());
        bytes
    }

    /// FC-PERSIST-POST-008(旧格式段四区为空 → 跳过磁盘索引,绝不误判损坏)
    #[test]
    fn empty_region_section_falls_back_to_rebuild() {
        let bytes = empty_legacy_msec();
        let view = msec::parse(&bytes).expect("旧格式段必须可解析");
        assert!(view.field_dict_bytes().is_empty());
        let mut state = WriterState::new();
        let loaded = load_disk_indexes(&mut state, &view, &[]).expect("空四区不是损坏");
        assert!(!loaded, "旧格式段应显式走全量重建");
    }

    /// FC-PERSIST-ERR-010(新区段四区结构畸形且 fail-fast → `Corrupted`)
    #[test]
    fn malformed_region_section_is_error() {
        // 非空 field_dict + 空 zmap:decode 阶段必然失败,调用方 fail-fast 时应上报。
        let fields = msec::encode_field_dict(&[(Arc::from("key"), msec::FieldKind::Str)]);
        let dict_offset = crate::persist::msec::HEADER_LEN as u64;
        let mut bytes = vec![0_u8; crate::persist::msec::HEADER_LEN as usize];
        bytes[0..4].copy_from_slice(b"MSC1");
        bytes[4..6].copy_from_slice(&crate::persist::FORMAT_VERSION.to_le_bytes());
        bytes[6..8].copy_from_slice(&crate::persist::msec::HEADER_LEN.to_le_bytes());
        // field_dict 指向紧随头部的数据区。
        bytes[16..24].copy_from_slice(&dict_offset.to_le_bytes());
        bytes[24..32].copy_from_slice(&(fields.len() as u64).to_le_bytes());
        let crc = crate::persist::crc32(&bytes[0..160]);
        bytes[160..164].copy_from_slice(&crc.to_le_bytes());
        bytes.extend_from_slice(&fields);
        bytes.extend_from_slice(&crate::persist::crc32(&fields).to_le_bytes());

        let view = msec::parse(&bytes).expect("段头本身合法");
        assert!(!view.field_dict_bytes().is_empty());
        let mut state = WriterState::new();
        let result = load_disk_indexes(&mut state, &view, &[]);
        assert!(
            matches!(result, Err(MnemeError::Corrupted { .. })),
            "畸形区结构必须报 Corrupted"
        );
    }

    /// FC-PERSIST-ERR-010(半新半旧:field_dict 空但其它索引区非空 → `Corrupted`)
    #[test]
    fn half_indexed_legacy_section_is_rejected() {
        let zmap = [0_u8; 8];
        let zmap_offset = crate::persist::msec::HEADER_LEN as u64;
        let mut bytes = vec![0_u8; crate::persist::msec::HEADER_LEN as usize];
        bytes[0..4].copy_from_slice(b"MSC1");
        bytes[4..6].copy_from_slice(&crate::persist::FORMAT_VERSION.to_le_bytes());
        bytes[6..8].copy_from_slice(&crate::persist::msec::HEADER_LEN.to_le_bytes());
        // zmap 的 offset/len 对在头部偏移 96/104(见 `encode_header`)。
        bytes[96..104].copy_from_slice(&zmap_offset.to_le_bytes());
        bytes[104..112].copy_from_slice(&(zmap.len() as u64).to_le_bytes());
        let crc = crate::persist::crc32(&bytes[0..160]);
        bytes[160..164].copy_from_slice(&crc.to_le_bytes());
        bytes.extend_from_slice(&zmap);
        bytes.extend_from_slice(&crate::persist::crc32(&zmap).to_le_bytes());

        let view = msec::parse(&bytes).expect("段头本身合法");
        assert!(view.field_dict_bytes().is_empty());
        let mut state = WriterState::new();
        assert!(
            matches!(
                load_disk_indexes(&mut state, &view, &[]),
                Err(MnemeError::Corrupted { .. })
            ),
            "半新半旧段必须按损坏拒绝,不得静默跳过校验"
        );
    }

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

    /// FC-PERSIST-ERR-009:映射按"(rowid, seqno) 有序链位置"重排(非恒等);
    /// 多段各自独立映射,互不影响。
    #[test]
    fn build_remaps_maps_slots_in_version_chain_order() {
        // 段内槽位 1 的版本排在链首、槽位 0 排在其后 → 映射必须非恒等。
        let versions = [(version(1, 1), 0), (version(2, 0), 0)];
        let remaps = build_remaps(&versions, &[2], &[0]).expect("合法布局不得报错");
        assert_eq!(remaps.len(), 1);
        assert_eq!(remaps[0].remap, vec![1, 0], "remap[段内槽位] = 有序链位置");

        // 多段:段 7 占全局槽位 0/1,段 8 的版本排在其后。
        let multi_versions = [(version(1, 1), 0), (version(2, 0), 0), (version(3, 0), 1)];
        let multi = build_remaps(&multi_versions, &[2, 1], &[7, 8]).expect("多段独立映射");
        assert_eq!(multi[0].segment_id, 7);
        assert_eq!(multi[0].remap, vec![1, 0]);
        assert_eq!(multi[1].segment_id, 8);
        assert_eq!(multi[1].remap, vec![2]);
    }

    /// FC-PERSIST-ERR-009:槽位越界、重复、存在未被版本行引用的槽位
    /// (vsec/msec 行数不一致)→ `Corrupted`,绝不静默映射到槽位 0。
    #[test]
    fn build_remaps_rejects_out_of_range_duplicate_or_unreferenced_slots() {
        // 槽位越界:slot 2 不在 [0, row_count)。
        let out_of_range = [(version(1, 2), 0)];
        assert!(matches!(
            build_remaps(&out_of_range, &[2], &[0]),
            Err(MnemeError::Corrupted { .. })
        ));

        // 槽位重复:两条版本行同占 slot 0。
        let duplicate = [(version(1, 0), 0), (version(2, 0), 0)];
        assert!(matches!(
            build_remaps(&duplicate, &[2], &[0]),
            Err(MnemeError::Corrupted { .. })
        ));

        // 未被引用:row_count = 2 但只有 slot 0 出现。
        let unreferenced = [(version(1, 0), 0)];
        assert!(matches!(
            build_remaps(&unreferenced, &[2], &[0]),
            Err(MnemeError::Corrupted { .. })
        ));
    }
}
