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
    let cutoff = cutoff_of(now_ms, horizon);
    let mut survivors = SurvivorSet::default();
    for (index, segment) in ws.slot_segment.iter().enumerate() {
        let Some(id) = segment else {
            continue;
        };
        if !in_plan.contains(id) {
            continue;
        }
        let window = Window { cutoff, now_ms };
        match classify_slot(ws, index, window, &|candidate| in_plan.contains(&candidate)) {
            SurvivorDecision::Reclaim => survivors.reclaim.push(index),
            SurvivorDecision::Keep => survivors.keep.push(index),
        }
    }
    survivors
}

/// 时间窗口截断点;`None` = 永久保留。
fn cutoff_of(now_ms: i64, horizon: Option<Duration>) -> Option<i64> {
    horizon
        .map(|window| now_ms.saturating_sub(i64::try_from(window.as_millis()).unwrap_or(i64::MAX)))
}

/// 回收时间窗口(截断点与当前时刻)。
#[derive(Clone, Copy)]
struct Window {
    /// `now - history_horizon`;`None` = 永久保留。
    cutoff: Option<i64>,
    /// 当前事务时刻(Unix 毫秒)。
    now_ms: i64,
}

/// 单个槽位在本轮 compaction 中的去留。
#[derive(PartialEq)]
enum SurvivorDecision {
    /// 写入新段。
    Keep,
    /// 提交后从版本链剪除并标死。
    Reclaim,
}

/// 判定单个槽位的去留;`in_scope` 决定某段是否属于本轮回收范围
/// (计划段组或单段统计,两种调用共用同一口径)。
fn classify_slot(
    ws: &WriterState,
    index: usize,
    window: Window,
    in_scope: &dyn Fn(u32) -> bool,
) -> SurvivorDecision {
    let slot = &ws.slots[index];
    let rowid = slot.rowid;
    let is_latest = ws
        .latest
        .get(&rowid)
        .is_some_and(|latest| latest.get() as usize == index);
    // 整链回收:回收范围覆盖全链,且最新版本为**窗口外**的墓碑/逻辑过期。
    // latest 与历史版本必须同进退——只回收 latest 会把旧活版本留在链上,
    // 造成被删记录"复活"或 `latest` 悬挂(FC-MODEL-POST-004;时钟回拨场景)。
    let chain_reclaimable =
        latest_dead_outside_window(ws, rowid, window) && chain_all_in_scope(ws, rowid, in_scope);
    if chain_reclaimable {
        return SurvivorDecision::Reclaim;
    }
    if is_latest {
        return SurvivorDecision::Keep;
    }
    match window.cutoff {
        Some(cutoff) if slot.tx_ms < cutoff => SurvivorDecision::Reclaim,
        _ => SurvivorDecision::Keep,
    }
}

/// 最新版本是否为窗口外的墓碑/逻辑过期(整链回收的充分条件之一)。
fn latest_dead_outside_window(
    ws: &WriterState,
    rowid: crate::core::types::RowId,
    window: Window,
) -> bool {
    let Some(slot) = ws.latest.get(&rowid) else {
        return false;
    };
    let latest = &ws.slots[slot.get() as usize];
    let dead = latest.deleted
        || latest
            .expires_at
            .is_some_and(|expires| expires <= window.now_ms);
    dead && window.cutoff.is_some_and(|cutoff| latest.tx_ms < cutoff)
}

/// 该 RowId 的整条版本链是否都落在回收范围内(否则回收会令旧版本在别段复活)。
fn chain_all_in_scope(
    ws: &WriterState,
    rowid: crate::core::types::RowId,
    in_scope: &dyn Fn(u32) -> bool,
) -> bool {
    let chain = ws
        .versions
        .get(&rowid)
        .map(|chain| chain.as_slice())
        .unwrap_or_default();
    chain.iter().all(|chain_slot| {
        ws.slot_segment
            .get(chain_slot.get() as usize)
            .and_then(|segment| *segment)
            .is_some_and(in_scope)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::core::meta::Meta;
    use crate::core::types::{NsId, RowId, SeqNo, SlotId};
    use crate::memory::table::SlotData;

    fn policy() -> CompactionPolicy {
        CompactionPolicy::default()
    }

    /// 构造一个带指定字段的物理槽位(仅本模块测试用)。
    fn slot(rowid: u64, seqno: u64, tx_ms: i64, deleted: bool) -> Arc<SlotData> {
        Arc::new(SlotData {
            rowid: RowId::new(rowid),
            ns_id: NsId::new(1),
            ns_path: Arc::from("n"),
            seqno: SeqNo::new(seqno),
            key: None,
            vector: crate::memory::lazy::VectorStorage::owned(Arc::from(
                vec![0.0_f32, 1.0].into_boxed_slice(),
            )),
            norm_sq: 1.0,
            text: None,
            text_hash: None,
            meta: Meta::Null,
            created_at: tx_ms,
            expires_at: None,
            importance: 0.5,
            confidence: 1.0,
            valid_from: tx_ms,
            valid_to: None,
            provenance: None,
            tx_ms,
            deleted,
        })
    }

    /// 组装写状态:每个槽位都属于 `segments[index]` 段,`latest` 为指定下标。
    fn state_with(slots: Vec<Arc<SlotData>>, latest: usize, segments: u32) -> WriterState {
        let mut ws = WriterState::new();
        for (index, data) in slots.iter().enumerate() {
            Arc::make_mut(&mut ws.slots).push(Arc::clone(data));
            ws.slot_segment.push(Some(segments));
            let chain = ws.versions.get_or_insert_default(data.rowid);
            Arc::make_mut(chain).push(SlotId::new(index as u32));
        }
        ws.latest
            .insert(slots[latest].rowid, SlotId::new(latest as u32));
        ws
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

    /// FC-MODEL-POST-004:时钟回拨下最新墓碑在窗口外时,整链一并回收,
    /// 不得只回收墓碑而把旧活版本留在链上(latest 悬挂/删除复活)。
    #[test]
    fn reclaims_whole_chain_when_latest_tombstone_outside_window() {
        // 先写活版本(tx=280),后写墓碑(tx=100,模拟时钟回拨)。
        let live = slot(7, 2, 280, false);
        let tomb = slot(7, 3, 100, true);
        let ws = state_with(vec![live, tomb], 1, 0);
        let plan = CompactionPlan { segments: vec![0] };
        let survivors = select_survivors(&ws, &plan, 300, Some(Duration::from_millis(50)));
        assert!(
            survivors.keep.is_empty(),
            "latest 回收必须整链回收,实际 keep={:?}",
            survivors.keep
        );
        assert_eq!(survivors.reclaim, vec![0, 1]);
    }

    /// FC-MODEL-POST-004:窗口内墓碑保留、窗口外历史版本回收(二者同链也不误伤)。
    #[test]
    fn keeps_latest_tombstone_inside_window_and_reclaims_history() {
        let live = slot(7, 1, 100, false);
        let tomb = slot(7, 2, 280, true);
        let ws = state_with(vec![live, tomb], 1, 0);
        let plan = CompactionPlan { segments: vec![0] };
        let survivors = select_survivors(&ws, &plan, 300, Some(Duration::from_millis(50)));
        assert_eq!(survivors.keep, vec![1], "窗口内墓碑必须保留");
        assert_eq!(survivors.reclaim, vec![0], "窗口外历史版本回收");
    }

    /// FC-LIFE-INV-008:死比率只计可回收版本;`horizon = None` 时无回收收益。
    #[test]
    fn dead_ratio_counts_only_reclaimable_versions() {
        let live = slot(7, 1, 100, false);
        let tomb = slot(7, 2, 280, true);
        let ws = state_with(vec![live, tomb], 1, 0);
        assert!(
            segment_dead_ratios(&ws, 300, None).is_empty(),
            "默认 horizon=None 时不得报告死比率(否则无限重写)"
        );
        let ratios = segment_dead_ratios(&ws, 300, Some(Duration::from_millis(50)));
        let ratio = ratios.get(&0).copied().expect("段 0 应有统计");
        assert!(
            (ratio - 0.5).abs() < 1e-6,
            "cutoff=250:历史活版本可回收、窗口内墓碑不可回收,实际 {ratio}"
        );
    }
}
