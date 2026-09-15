use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::core::meta::Meta;
use crate::core::options::CompactionPolicy;
use crate::core::types::{NsId, RowId, SeqNo, SlotId};
use crate::memory::ops::CompactionPlan;
use crate::memory::table::{SlotData, WriterState};

use super::plan::level_of;
use super::*;

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
