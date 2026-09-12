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
use crate::memory::table::{SlotData, WriteOp, WriterState};
use crate::memory::write_helpers::{latest_live, validate_patch};

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
    // 全量校验先行:任一字段超限/非有限值 → 整个补丁拒绝,记录保持原版本(零部分写入)。
    validate_patch(config, patch)?;
    let now = config.clock.now_unix_ms();
    // 版本数据经 `Arc` 与读者共享,更新必须克隆出新版本(COW)。
    let mut slot_data = (*base).clone();
    apply_patch(&mut slot_data, patch, now);
    slot_data.seqno = ws.alloc_seqno();
    slot_data.tx_ms = now;
    slot_data.deleted = false;
    ws.commit_version(rowid, slot_data)?;
    Ok(UpdateOutcome::Updated(rowid))
}

/// 把补丁字段应用到版本数据上(全部字段已由 `validate_patch` 校验,不可失败)。
fn apply_patch(slot_data: &mut SlotData, patch: &UpdatePatch, now: i64) {
    if let Some(vector) = &patch.vector {
        apply_vector_patch(slot_data, vector);
    }
    apply_scalar_patch(slot_data, patch, now);
}

/// 应用补丁的向量字段:整体替换并重算范数平方(维度/有限值已由校验保证)。
fn apply_vector_patch(slot_data: &mut SlotData, vector: &[f32]) {
    slot_data.vector = Arc::from(vector.to_vec().into_boxed_slice());
    slot_data.norm_sq = search::norm_sq(&slot_data.vector);
}

/// 应用补丁的文本/元数据/标量字段(时间语义经 `now` 注入,不可失败)。
fn apply_scalar_patch(slot_data: &mut SlotData, patch: &UpdatePatch, now: i64) {
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
}

pub(crate) fn touch_rowid(
    ws: &mut WriterState,
    rowid: RowId,
    boost: Option<f32>,
    now: i64,
) -> Result<bool> {
    // 非有限值 boost 会污染 importance 与综合打分,入口直接拒绝(FC-GLOBAL-PRE-004)。
    if let Some(boost) = boost
        && !boost.is_finite()
    {
        return Err(MnemeError::NonFinite);
    }
    let Some(base) = latest_live(ws, rowid) else {
        return Ok(false);
    };
    // 墓碑/逻辑过期记录不可被强化,与 `feedback`/读路径同口径(FC-MEM-POST-009)。
    if !base.is_live(now) {
        return Ok(false);
    }
    if let Some(boost) = boost {
        let mut slot_data = (*base).clone();
        slot_data.importance = (slot_data.importance + boost).clamp(0.0, 1.0);
        slot_data.seqno = ws.alloc_seqno();
        slot_data.tx_ms = now;
        ws.commit_version(rowid, slot_data)?;
    }
    // 提交成功后再记访问,避免提交失败时计数被提前累加。
    {
        let stat = Arc::make_mut(&mut ws.access).entry(rowid).or_default();
        stat.access_count = stat.access_count.saturating_add(1);
        stat.last_access_ms = now;
    }
    // 访问增量随下次 flush 以 delta 区 `Access` 条目落盘(读路径零写放大)。
    ws.mark_access_dirty(rowid);
    // 访问统计单独落 WAL(重要性变更已随上面的版本 `Insert` 记录)。
    let seqno = ws.alloc_seqno();
    ws.pending.push(WriteOp::Access {
        rowid,
        seqno,
        at_ms: now,
        access_delta: 1,
        importance_delta: 0.0,
    });
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
