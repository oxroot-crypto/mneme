//! size-tiered compaction 计划与幸存版本筛选(设计 07 §4)。
//!
//! - **选段**:同层段数 ≥ `tier_count` 时合并该层最小的 `tier_count` 个段;
//!   任一段墓碑+过期占比 > `dead_ratio` 时优先单独重写该段(回收空间)。
//! - **幸存筛选**:全局最新版本(含最新墓碑)始终保留;历史版本在
//!   `history_horizon` 窗口内保留(默认永久);整链为超期墓碑/整链逻辑过期
//!   且窗口外时回收整个 RowId。逐版本 TTL 过滤会令旧版本复活,故过期回收
//!   必须整链判定(设计 07 §4.2a 的正确性口径)。

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::core::options::CompactionPolicy;
use crate::memory::ops::CompactionPlan;
use crate::memory::table::WriterState;

/// 单个活跃段的尺寸信息(计划输入;与持久层类型解耦)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SegmentInfo {
    /// 段编号。
    pub(crate) id: u32,
    /// 段内物理行数(含墓碑与历史版本)。
    pub(crate) rows: u64,
}

/// 幸存版本筛选结果(`slots` 下标)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SurvivorSet {
    /// 写入合并段的槽位(升序)。
    pub(crate) keep: Vec<usize>,
    /// 物理回收的槽位(升序;提交后从版本链剪除)。
    pub(crate) reclaim: Vec<usize>,
}

/// 由段尺寸与死比率选出本轮 compaction 段组。
///
/// 返回 `None` 表示无触发条件满足(不产新段)。
pub(crate) fn plan(
    segments: &[SegmentInfo],
    dead: &HashMap<u32, f32>,
    policy: &CompactionPolicy,
) -> Option<CompactionPlan> {
    if segments.is_empty() {
        return None;
    }
    // 死比率触发:选比率最高且超线的段单独重写(段数不变,空间回收)。
    let active: HashSet<u32> = segments.iter().map(|segment| segment.id).collect();
    if let Some((id, _ratio)) = dead
        .iter()
        .filter(|(id, ratio)| active.contains(id) && **ratio > policy.dead_ratio)
        .max_by(|left, right| left.1.total_cmp(right.1))
    {
        return Some(CompactionPlan {
            segments: vec![*id],
        });
    }

    // 分级:level 0 = 行数 < B;level n = B·r^(n-1) ≤ 行数 < B·r^n。
    let mut by_level: HashMap<u32, Vec<&SegmentInfo>> = HashMap::new();
    for segment in segments {
        by_level
            .entry(level_of(segment.rows, policy))
            .or_default()
            .push(segment);
    }
    let mut levels: Vec<u32> = by_level.keys().copied().collect();
    levels.sort_unstable();
    for level in levels {
        let group = by_level.get_mut(&level)?;
        let threshold = policy.tier_count.max(2) as usize;
        if group.len() >= threshold {
            group.sort_by_key(|segment| segment.rows);
            group.truncate(threshold);
            return Some(CompactionPlan {
                segments: group.iter().map(|segment| segment.id).collect(),
            });
        }
    }
    None
}

/// 由行数计算 size-tiered 层级。
fn level_of(rows: u64, policy: &CompactionPolicy) -> u32 {
    let base = policy.segment_rows.max(1);
    if rows < base {
        return 0;
    }
    let ratio = u64::from(policy.tier_ratio.max(2));
    let mut level = 1_u32;
    let mut bound = base;
    while bound <= rows / ratio {
        bound = bound.saturating_mul(ratio);
        level = level.saturating_add(1);
    }
    level
}

/// 统计每段"墓碑 + 逻辑过期"占比(计划触发用)。
pub(crate) fn segment_dead_ratios(ws: &WriterState, now_ms: i64) -> HashMap<u32, f32> {
    let mut dead: HashMap<u32, u64> = HashMap::new();
    let mut total: HashMap<u32, u64> = HashMap::new();
    for (index, segment) in ws.slot_segment.iter().enumerate() {
        let Some(id) = segment else {
            continue;
        };
        *total.entry(*id).or_insert(0) += 1;
        let slot = &ws.slots[index];
        if ws.dead.get(index) || slot.deleted || !slot.is_live(now_ms) {
            *dead.entry(*id).or_insert(0) += 1;
        }
    }
    total
        .into_iter()
        .map(|(id, count)| {
            let dead_count = dead.get(&id).copied().unwrap_or(0);
            let ratio = if count == 0 {
                0.0
            } else {
                dead_count as f32 / count as f32
            };
            (id, ratio)
        })
        .collect()
}

/// 从计划段组中筛选幸存版本。
///
/// `keep` 之外的槽位在段提交成功后由调用方从版本链剪除并标死。
pub(crate) fn select_survivors(
    ws: &WriterState,
    plan: &CompactionPlan,
    now_ms: i64,
    horizon: Option<Duration>,
) -> SurvivorSet {
    let in_plan: HashSet<u32> = plan.segments.iter().copied().collect();
    let cutoff = horizon
        .map(|window| now_ms.saturating_sub(i64::try_from(window.as_millis()).unwrap_or(i64::MAX)));
    let mut survivors = SurvivorSet::default();
    for (index, segment) in ws.slot_segment.iter().enumerate() {
        let Some(id) = segment else {
            continue;
        };
        if !in_plan.contains(id) {
            continue;
        }
        let slot = &ws.slots[index];
        let rowid = slot.rowid;
        let chain = ws
            .versions
            .get(&rowid)
            .map(Vec::as_slice)
            .unwrap_or_default();
        // 整链都在本轮段组内才允许整链回收;否则旧版本可能存活于其他段而复活。
        let all_in_group = chain.iter().all(|chain_slot| {
            ws.slot_segment
                .get(chain_slot.get() as usize)
                .and_then(|segment| *segment)
                .is_some_and(|segment_id| in_plan.contains(&segment_id))
        });
        let latest = ws
            .latest
            .get(&rowid)
            .map(|slot| &ws.slots[slot.get() as usize]);
        let chain_reclaimable = all_in_group
            && cutoff.is_some()
            && latest.is_some_and(|latest| {
                let expired = latest.expires_at.is_some_and(|expires| expires <= now_ms);
                latest.deleted || expired
            });
        let is_latest = ws
            .latest
            .get(&rowid)
            .is_some_and(|latest| latest.get() as usize == index);
        if chain_reclaimable
            && let Some(cutoff) = cutoff
            && slot.tx_ms < cutoff
        {
            survivors.reclaim.push(index);
            continue;
        }
        if is_latest {
            survivors.keep.push(index);
            continue;
        }
        match cutoff {
            Some(cutoff) if slot.tx_ms < cutoff => survivors.reclaim.push(index),
            _ => survivors.keep.push(index),
        }
    }
    survivors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> CompactionPolicy {
        CompactionPolicy::default()
    }

    /// FC-LIFE-CPLX-004:同层攒够 `tier_count` 即选最小的一组。
    #[test]
    fn plan_merges_smallest_same_level_segments() {
        let segments: Vec<SegmentInfo> = (0..5).map(|id| SegmentInfo { id, rows: 8_192 }).collect();
        let plan = plan(&segments, &HashMap::new(), &policy()).expect("同层达标必须触发");
        assert_eq!(plan.segments.len(), 4);
        assert_eq!(plan.segments, vec![0, 1, 2, 3], "应选编号稳定的一组");
    }

    /// FC-LIFE-INV-008:不同层不混并;不足阈值不触发。
    #[test]
    fn plan_keeps_levels_separate_and_below_threshold_quiet() {
        let segments = vec![
            SegmentInfo { id: 0, rows: 100 },
            SegmentInfo { id: 1, rows: 8_192 },
            SegmentInfo {
                id: 2,
                rows: 8_192 * 4,
            },
        ];
        assert!(plan(&segments, &HashMap::new(), &policy()).is_none());
    }

    /// FC-LIFE-INV-008:死比率超线优先单独重写该段。
    #[test]
    fn plan_prefers_dead_ratio_victim() {
        let segments = vec![
            SegmentInfo { id: 0, rows: 100 },
            SegmentInfo { id: 1, rows: 100 },
        ];
        let dead = HashMap::from([(1, 0.5_f32)]);
        let plan = plan(&segments, &dead, &policy()).expect("死比率触发");
        assert_eq!(plan.segments, vec![1]);
    }

    /// 分级边界:B/r 的整数边界与上界。
    #[test]
    fn level_boundaries_follow_ratio() {
        let policy = policy();
        assert_eq!(level_of(1, &policy), 0);
        assert_eq!(level_of(8_191, &policy), 0);
        assert_eq!(level_of(8_192, &policy), 1);
        assert_eq!(level_of(8_192 * 4 - 1, &policy), 1);
        assert_eq!(level_of(8_192 * 4, &policy), 2);
    }
}
