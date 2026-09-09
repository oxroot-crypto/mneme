//! 写后变更辅助:更新、访问强化与沉淀摘要(`mutate_helpers.rs`)。
//!
//! 从 `write_helpers.rs` 拆出的 `WriterState` 变更操作;不构成公开 API。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::options::{RelationKind, UpdatePatch};
use crate::core::types::RowId;
use crate::memory::config::Config;
use crate::memory::dedup;
use crate::memory::record::{Record, RecordRef, UpdateOutcome};
use crate::memory::score::ConsolidationPolicy;
use crate::memory::search;
use crate::memory::table::{SlotData, WriterState};
use crate::memory::write_helpers::latest_live;

/// `Feedback::Corrected` 降低可信度的步长。
const CONFIDENCE_DECAY_STEP: f32 = 0.1;
/// 按 `RowId` 局部更新,写入新物理版本(不改变 `RowId`)。
pub(crate) fn update_rowid(
    ws: &mut WriterState,
    config: &Config,
    rowid: RowId,
    patch: &UpdatePatch,
) -> Result<UpdateOutcome> {
    let Some(base) = latest_live(ws, rowid) else {
        return Ok(UpdateOutcome::NotFound);
    };
    let now = config.clock.now_unix_ms();
    // 版本数据经 `Arc` 与读者共享,更新必须克隆出新版本(COW)。
    let mut slot_data = (*base).clone();
    apply_patch(&mut slot_data, patch, config, now)?;
    slot_data.seqno = ws.alloc_seqno();
    slot_data.tx_ms = now;
    slot_data.deleted = false;
    ws.commit_version(rowid, slot_data)?;
    Ok(UpdateOutcome::Updated(rowid))
}

/// 把补丁字段应用到版本数据上。
fn apply_patch(
    slot_data: &mut SlotData,
    patch: &UpdatePatch,
    config: &Config,
    now: i64,
) -> Result<()> {
    if let Some(vector) = &patch.vector {
        if vector.len() != config.dimension.get() as usize {
            return Err(MnemeError::DimensionMismatch {
                expected: config.dimension.get(),
                got: vector.len(),
            });
        }
        if vector.iter().any(|value| !value.is_finite()) {
            return Err(MnemeError::NonFinite);
        }
        slot_data.vector = Arc::from(vector.clone().into_boxed_slice());
        slot_data.norm_sq = search::norm_sq(&slot_data.vector);
    }
    if let Some(text) = &patch.text {
        match text {
            Some(text) => {
                slot_data.text = Some(Arc::from(text.as_str()));
                slot_data.text_hash = Some(dedup::fnv1a64(text.as_bytes()));
            }
            None => {
                slot_data.text = None;
                slot_data.text_hash = None;
            }
        }
    }
    if let Some(metadata) = &patch.metadata {
        slot_data.meta = metadata.clone().unwrap_or(Meta::Null);
    }
    if let Some(importance) = patch.importance {
        slot_data.importance = importance.clamp(0.0, 1.0);
    }
    if let Some(ttl) = &patch.ttl {
        slot_data.expires_at = ttl.map(|ttl| now.saturating_add(ttl.as_millis() as i64));
    }
    if let Some((valid_from, valid_to)) = &patch.valid_time {
        slot_data.valid_from = *valid_from;
        slot_data.valid_to = *valid_to;
    }
    if let Some(confidence) = patch.confidence {
        slot_data.confidence = confidence.clamp(0.0, 1.0);
    }
    if let Some(provenance) = &patch.provenance {
        slot_data.provenance = provenance.clone();
    }
    Ok(())
}

pub(crate) fn touch_rowid(
    ws: &mut WriterState,
    rowid: RowId,
    boost: Option<f32>,
    now: i64,
) -> Result<bool> {
    let Some(base) = latest_live(ws, rowid) else {
        return Ok(false);
    };
    let stat = Arc::make_mut(&mut ws.access).entry(rowid).or_default();
    stat.access_count = stat.access_count.saturating_add(1);
    stat.last_access_ms = now;
    if let Some(boost) = boost {
        let mut slot_data = (*base).clone();
        slot_data.importance = (slot_data.importance + boost).clamp(0.0, 1.0);
        slot_data.seqno = ws.alloc_seqno();
        slot_data.tx_ms = now;
        ws.commit_version(rowid, slot_data)?;
    }
    Ok(true)
}

pub(crate) fn lower_confidence(ws: &mut WriterState, rowid: RowId, now: i64) -> Result<()> {
    if let Some(base) = latest_live(ws, rowid) {
        let mut slot_data = (*base).clone();
        slot_data.confidence = (slot_data.confidence - CONFIDENCE_DECAY_STEP).clamp(0.0, 1.0);
        slot_data.seqno = ws.alloc_seqno();
        slot_data.tx_ms = now;
        ws.commit_version(rowid, slot_data)?;
    }
    Ok(())
}

pub(crate) fn is_consolidated(ws: &WriterState, rowid: RowId) -> bool {
    ws.in_edges.get(&rowid).is_some_and(|edges| {
        edges
            .iter()
            .any(|edge| edge.kind == RelationKind::DERIVED_FROM)
    })
}

pub(crate) fn build_summary(policy: &ConsolidationPolicy, members: &[&Arc<SlotData>]) -> Record {
    if let Some(summarizer) = &policy.summarizer {
        let refs: Vec<RecordRef<'_>> = members
            .iter()
            .map(|slot_data| RecordRef::new(Arc::clone(slot_data)))
            .collect();
        if let Some(record) = summarizer.summarize(&refs) {
            return record;
        }
    }
    let best = members
        .iter()
        .max_by(|a, b| {
            a.importance
                .partial_cmp(&b.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .copied()
        .unwrap_or(members[0]);
    let mut texts: Vec<String> = Vec::new();
    for member in members {
        if let Some(text) = member.text.as_deref()
            && !texts.iter().any(|existing| existing == text)
        {
            texts.push(text.to_string());
        }
    }
    let confidence = members
        .iter()
        .map(|slot_data| slot_data.confidence)
        .sum::<f32>()
        / members.len() as f32;
    let mut record = Record::new(best.vector.to_vec()).importance(best.importance);
    if !texts.is_empty() {
        record = record.text(texts.join("; "));
    }
    record.confidence(confidence)
}
