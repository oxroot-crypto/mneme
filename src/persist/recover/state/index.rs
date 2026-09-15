//! 盘上四区索引个装载同全量重建降级。

use crate::core::error::{MnemeError, Result};
use crate::memory::table::WriterState;
use crate::persist::msec;

use super::types::{ParsedSegment, SegmentRemap};

/// 载入或重建检索加速结构(倒排 / zone map / bloom)。
///
/// 单段且存在重排映射时优先用段内四区装载倒排(免重新分词);多段时逐段解码倒排
/// 并按全局槽位合并,bloom/zone map 从槽位重建;结构不合法时 `fail_fast` 报错,
/// 否则降级为从槽位全量重建——两条路径产生等价的索引。
pub(super) fn apply_indexes(
    state: &mut WriterState,
    parsed: &[ParsedSegment<'_>],
    remaps: &[SegmentRemap],
    fail_fast: bool,
) -> Result<()> {
    if parsed.is_empty() {
        return Ok(());
    }
    if let ([segment], [remap]) = (parsed, remaps) {
        match load_disk_indexes(state, &segment.msec_view, &remap.remap) {
            // 四区结构校验通过:直接复用磁盘索引。
            Ok(()) => return Ok(()),
            Err(error) if fail_fast => return Err(error),
            // 索引是查询加速器而非数据来源:损坏时降级全量重建仍然正确。
            Err(_) => {}
        }
        state.rebuild_indexes();
        return Ok(());
    }

    // 多段:逐段解码倒排(经各自重排映射)并合并;任一结构不一致即整库重建。
    let mut merged = crate::memory::analysis::InvertedIndex::default();
    for (segment, remap) in parsed.iter().zip(remaps.iter()) {
        match decode_segment_index(&segment.msec_view, &remap.remap) {
            Ok(inv) => merged.merge_from(inv),
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

/// 校验单段四区结构并解码倒排(多段恢复路径)。
///
/// 与单段路径同口径:字段字典/zone map/`ttl_map`/bloom 全部校验;任一畸形返回
/// `Corrupted`(调用方按 `fail_fast` 决定上报或降级重建)。
fn decode_segment_index(
    msec_view: &msec::MsecView<'_>,
    remap: &[u32],
) -> Result<crate::memory::analysis::InvertedIndex> {
    let fields = msec::decode_field_dict(msec_view.field_dict_bytes())?;
    let block_count =
        (msec_view.row_count() as usize).div_ceil(crate::memory::analysis::ZONE_BLOCK_ROWS);
    msec::validate_zmap(msec_view.zmap_bytes(), &fields, block_count)?;
    let _ttl_map = msec::decode_ttl_map(msec_view.zmap_bytes(), &fields, block_count)?;
    msec::decode_bloom(msec_view.bloom_bytes())?;
    let inv = msec::decode_inverted(msec_view.inverted_bytes(), remap)?;
    Ok(inv)
}

/// 由段内四区装载索引:字段字典/zone map 校验,倒排经重排映射,bloom 直接复用。
///
/// 四区为当前格式必填;任一缺失/畸形返回 `Corrupted`(调用方按 `fail_fast`
/// 决定上报或降级重建)。
pub(super) fn load_disk_indexes(
    state: &mut WriterState,
    msec_view: &msec::MsecView<'_>,
    remap: &[u32],
) -> Result<()> {
    let fields = msec::decode_field_dict(msec_view.field_dict_bytes())?;
    validate_disk_regions(state, msec_view, &fields)?;
    let key_bloom = decode_key_bloom(msec_view, &fields)?;
    let inv = msec::decode_inverted(msec_view.inverted_bytes(), remap)?;
    state.load_disk_indexes(inv, key_bloom);
    Ok(())
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
