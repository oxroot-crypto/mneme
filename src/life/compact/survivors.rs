//! 幸存版本筛选:逐槽位判定保留/回收,整链回收口径(设计 07 §4.2a)。

use std::collections::HashSet;
use std::time::Duration;

use crate::memory::ops::CompactionPlan;
use crate::memory::table::WriterState;

/// 幸存版本筛选结果(`slots` 下标)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SurvivorSet {
    /// 写入合并段的槽位(升序)。
    pub(crate) keep: Vec<usize>,
    /// 物理回收的槽位(升序;提交后从版本链剪除)。
    pub(crate) reclaim: Vec<usize>,
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
pub(super) fn cutoff_of(now_ms: i64, horizon: Option<Duration>) -> Option<i64> {
    horizon
        .map(|window| now_ms.saturating_sub(i64::try_from(window.as_millis()).unwrap_or(i64::MAX)))
}

/// 回收时间窗口(截断点与当前时刻)。
#[derive(Clone, Copy)]
pub(super) struct Window {
    /// `now - history_horizon`;`None` = 永久保留。
    pub(super) cutoff: Option<i64>,
    /// 当前事务时刻(Unix 毫秒)。
    pub(super) now_ms: i64,
}

/// 单个槽位在本轮 compaction 中的去留。
#[derive(PartialEq)]
pub(super) enum SurvivorDecision {
    /// 写入新段。
    Keep,
    /// 提交后从版本链剪除并标死。
    Reclaim,
}

/// 判定单个槽位的去留;`in_scope` 决定某段是否属于本轮回收范围
/// (计划段组或单段统计,两种调用共用同一口径)。
pub(super) fn classify_slot(
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
