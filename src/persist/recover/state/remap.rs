//! "段内槽位 → 全局槽位"重排映射同槽位段回填。

use crate::core::error::{MnemeError, Result};
use crate::memory::table::WriterState;
use crate::persist::msec::VersionRow;

use super::types::SegmentRemap;

/// 回填"槽位 → 所属段"归属。
///
/// 恢复出的槽位都属于其来源段,供增量 flush 与 compaction 辨识已落盘数据;
/// 缺失会让 `unpersisted_slots` 把全部活跃槽位当作未落盘,重开后的 compaction
/// 会以空段替换活跃段集(永久丢数据,FC-PERSIST-POST-012)。
pub(super) fn backfill_slot_segments(state: &mut WriterState, remaps: &[SegmentRemap]) {
    for remap in remaps {
        for &global in &remap.remap {
            if let Some(entry) = state.slot_segment.get_mut(global as usize) {
                *entry = Some(remap.segment_id);
            }
        }
    }
}

/// 为每个已解析段构建"段内槽位 → 全局槽位"重排映射。
///
/// 第 k 个被应用的版本落入全局槽位 k(槽位从空开始),故
/// `remap[段内槽位] = 该版本在 (rowid, seqno) 有序链中的位置`。多段各自独立映射。
///
/// # Errors
/// 版本槽位越界/重复,或存在未被版本行引用的段内槽位(vsec/msec 行数不一致)时
/// 返回 [`MnemeError::Corrupted`]——继续使用会把未引用槽位静默映射到槽位 0。
pub(super) fn build_remaps(
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
