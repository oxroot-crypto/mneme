//! size-tiered 选段与死比率统计(设计 07 §4.1/§4.2)。

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::core::options::CompactionPolicy;
use crate::memory::ops::CompactionPlan;
use crate::memory::table::WriterState;

use super::survivors::{SurvivorDecision, Window, classify_slot, cutoff_of};

/// 单个活跃段的尺寸信息(计划输入;与持久层类型解耦)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SegmentInfo {
    /// 段编号。
    pub(crate) id: u32,
    /// 段内物理行数(含墓碑与历史版本)。
    pub(crate) rows: u64,
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
    // 同比率时以段号小者优先,保证选段确定性。
    let active: HashSet<u32> = segments.iter().map(|segment| segment.id).collect();
    if let Some((id, _ratio)) = dead
        .iter()
        .filter(|(id, ratio)| active.contains(id) && **ratio > policy.dead_ratio)
        .max_by(|left, right| left.1.total_cmp(right.1).then_with(|| right.0.cmp(left.0)))
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
        // level 来自 `by_level.keys()`,键必然存在;缺失即内部不变量被破坏,
        // 防御性跳过该层(不产计划)而非 panic。
        let Some(group) = by_level.get_mut(&level) else {
            continue;
        };
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
pub(super) fn level_of(rows: u64, policy: &CompactionPolicy) -> u32 {
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

/// 统计每段"可回收死行"占比(计划触发用)。
///
/// 只计 `history_horizon` 窗口外、本层真正能回收的版本:
/// - `horizon = None`(默认永久保留)时没有任何可回收死行,返回空表——否则死比率
///   触发会反复重写却不减死行,形成无限写放大;
/// - 窗口内的墓碑/过期与窗口内的历史版本均不计。
pub(crate) fn segment_dead_ratios(
    ws: &WriterState,
    now_ms: i64,
    horizon: Option<Duration>,
) -> HashMap<u32, f32> {
    let Some(cutoff) = cutoff_of(now_ms, horizon) else {
        return HashMap::new();
    };
    let mut dead: HashMap<u32, u64> = HashMap::new();
    let mut total: HashMap<u32, u64> = HashMap::new();
    for (index, segment) in ws.slot_segment.iter().enumerate() {
        let Some(id) = segment else {
            continue;
        };
        *total.entry(*id).or_insert(0) += 1;
        // 与计划共用同一回收口径(含"整链在该段内"判定),保证触发条件可达:
        // 单段计划只回收链全在本段的版本,统计也必须按同口径计数,否则跨段
        // 版本链会反复触发却零回收(无限写放大,FC-LIFE-INV-008)。
        let window = Window {
            cutoff: Some(cutoff),
            now_ms,
        };
        if classify_slot(ws, index, window, &|candidate| candidate == *id)
            == SurvivorDecision::Reclaim
        {
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
