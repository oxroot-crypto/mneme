//! 写路径内部辅助(`write_helpers.rs`)。
//!
//! 全部为作用于 `WriterState` 的自由函数,供 `Namespace` 写/生命周期方法与
//! `consolidate` 复用;不构成公开 API。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::{self, Meta};
use crate::core::options::{InsertMode, UpdatePatch};
use crate::core::types::{Key, NsId, RowId, SeqNo};
use crate::memory::config::Config;
use crate::memory::dedup::{self, Dedup};
use crate::memory::record::{InsertOutcome, Record, RecordRef};
use crate::memory::score;
use crate::memory::search;
use crate::memory::table::{SlotData, WriterState};

/// 未指定重要度时的缺省值(`[0,1]` 归一区间)。
const DEFAULT_IMPORTANCE: f32 = 0.5;

/// 未指定可信度时的缺省值(`[0,1]` 归一区间)。
const DEFAULT_CONFIDENCE: f32 = 1.0;

/// 构造一个物理版本的输入(按值持有记录,避免克隆向量)。
pub(crate) struct SlotSpec {
    /// 目标命名空间。
    pub(crate) ns_id: NsId,
    /// 命名空间路径。
    pub(crate) ns_path: Arc<str>,
    /// 稳定逻辑标识。
    pub(crate) rowid: RowId,
    /// 版本序号。
    pub(crate) seqno: SeqNo,
    /// 事务时间(Unix 毫秒)。
    pub(crate) tx_ms: i64,
    /// 来源记录(按值消费)。
    pub(crate) rec: Record,
}

/// 单条写入的输入上下文(按值持有记录)。
pub(crate) struct InsertCtx {
    /// 目标命名空间。
    pub(crate) ns_id: NsId,
    /// 命名空间路径。
    pub(crate) ns_path: Arc<str>,
    /// 待写入记录。
    pub(crate) rec: Record,
    /// 当前时刻(Unix 毫秒)。
    pub(crate) now: i64,
    /// 是否处于批量写入(影响 `RejectDuplicate` 的返回形式)。
    pub(crate) in_batch: bool,
}

/// 校验向量维度与分量有限性(写/更新同口径)。
fn validate_vector(config: &Config, vector: &[f32]) -> Result<()> {
    let expected = config.dimension.get();
    if vector.len() != expected as usize {
        return Err(MnemeError::DimensionMismatch {
            expected,
            got: vector.len(),
        });
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(MnemeError::NonFinite);
    }
    Ok(())
}

/// 校验 `importance`/`confidence` 标量因子为有限值(非有限值会污染打分与遗忘公式)。
fn validate_scalars(importance: Option<f32>, confidence: Option<f32>) -> Result<()> {
    // 拒绝而非静默钳制(FC-GLOBAL-PRE-004)。
    if importance.is_some_and(|value| !value.is_finite())
        || confidence.is_some_and(|value| !value.is_finite())
    {
        return Err(MnemeError::NonFinite);
    }
    Ok(())
}

/// 校验单个 metadata/provenance 字段的大小与嵌套深度(FC-GLOBAL-PRE-003)。
fn validate_meta_field(config: &Config, field: &'static str, meta: &Meta) -> Result<()> {
    let size = meta::size_bytes(meta);
    if size > config.limits.meta_bytes {
        return Err(MnemeError::TooLarge {
            field,
            limit: config.limits.meta_bytes,
            got: size,
        });
    }
    let depth = meta::depth(meta);
    if depth > config.limits.meta_depth as usize {
        return Err(MnemeError::MetaTooDeep {
            limit: config.limits.meta_depth as usize,
            got: depth,
        });
    }
    Ok(())
}

/// 写入前校验维度、有限性与各项限额。
pub(crate) fn validate_insert(config: &Config, rec: &Record) -> Result<()> {
    validate_vector(config, &rec.vector)?;
    validate_scalars(rec.importance, rec.confidence)?;
    if let Some(key) = &rec.key
        && key.len() > config.limits.key_bytes
    {
        return Err(MnemeError::TooLarge {
            field: "key",
            limit: config.limits.key_bytes,
            got: key.len(),
        });
    }
    if let Some(text) = &rec.text
        && text.len() > config.limits.text_bytes
    {
        return Err(MnemeError::TooLarge {
            field: "text",
            limit: config.limits.text_bytes,
            got: text.len(),
        });
    }
    for (field, meta) in [("metadata", &rec.metadata), ("provenance", &rec.provenance)] {
        if let Some(meta) = meta {
            validate_meta_field(config, field, meta)?;
        }
    }
    Ok(())
}

/// 校验更新补丁:向量维度/有限值与 text/metadata/provenance 限额(FC-MEM-PRE-002,
/// 与 [`validate_insert`] 同口径;`Some(None)` 清空语义不携带新载荷,无需校验)。
pub(crate) fn validate_patch(config: &Config, patch: &UpdatePatch) -> Result<()> {
    if let Some(vector) = &patch.vector {
        validate_vector(config, vector)?;
    }
    validate_scalars(patch.importance, patch.confidence)?;
    if let Some(Some(text)) = &patch.text
        && text.len() > config.limits.text_bytes
    {
        return Err(MnemeError::TooLarge {
            field: "text",
            limit: config.limits.text_bytes,
            got: text.len(),
        });
    }
    for (field, meta) in [
        ("metadata", &patch.metadata),
        ("provenance", &patch.provenance),
    ] {
        if let Some(Some(meta)) = meta {
            validate_meta_field(config, field, meta)?;
        }
    }
    Ok(())
}

/// 由输入构造一个物理版本(消费记录,向量零拷贝转移)。
pub(crate) fn build_slot(spec: SlotSpec) -> SlotData {
    let vector: Arc<[f32]> = Arc::from(spec.rec.vector.into_boxed_slice());
    let norm_sq = search::norm_sq(&vector);
    let text: Option<Arc<str>> = spec.rec.text.map(Arc::from);
    let text_hash = text.as_ref().map(|text| dedup::fnv1a64(text.as_bytes()));
    let expires_at = spec
        .rec
        .ttl
        .map(|ttl| spec.tx_ms.saturating_add(ttl.as_millis() as i64));
    SlotData {
        rowid: spec.rowid,
        ns_id: spec.ns_id,
        ns_path: spec.ns_path,
        seqno: spec.seqno,
        key: spec.rec.key.map(Key::new),
        vector,
        norm_sq,
        text,
        text_hash,
        meta: spec.rec.metadata.unwrap_or(Meta::Null),
        created_at: spec.tx_ms,
        expires_at,
        importance: spec
            .rec
            .importance
            .unwrap_or(DEFAULT_IMPORTANCE)
            .clamp(0.0, 1.0),
        confidence: spec
            .rec
            .confidence
            .unwrap_or(DEFAULT_CONFIDENCE)
            .clamp(0.0, 1.0),
        valid_from: spec.rec.valid_from.unwrap_or(spec.tx_ms),
        valid_to: spec.rec.valid_to,
        provenance: spec.rec.provenance,
        tx_ms: spec.tx_ms,
        deleted: false,
    }
}

pub(crate) fn latest_live(ws: &WriterState, rowid: RowId) -> Option<Arc<SlotData>> {
    let slot = *ws.latest.get(&rowid)?;
    let slot_data = Arc::clone(&ws.slots[slot.get() as usize]);
    (!slot_data.deleted).then_some(slot_data)
}

/// 判重探针的只读上下文(聚合配置/命名空间/时刻,避免超长参数列表)。
pub(crate) struct DedupProbe<'a> {
    /// 建库配置(提供 `dedup_threshold`)。
    pub(crate) config: &'a Config,
    /// 目标命名空间。
    pub(crate) ns_id: NsId,
    /// 当前时刻(Unix 毫秒)。
    pub(crate) now: i64,
}

/// 查找与 `rec` 命中的既有记录;返回 `(rowid, 相似度)`。
pub(crate) fn find_duplicate(
    ws: &WriterState,
    probe: DedupProbe<'_>,
    rec: &Record,
) -> Option<(RowId, f32)> {
    if let Some(text) = &rec.text {
        let hash = dedup::fnv1a64(text.as_bytes());
        if let Some(rowid) = ws.text_index.get(&(probe.ns_id, hash)).copied()
            && let Some(slot_data) = latest_live(ws, rowid)
            && slot_data.is_live(probe.now)
            && slot_data.text.as_deref() == Some(text.as_str())
        {
            return Some((rowid, 1.0));
        }
    }
    let mut best: Option<(RowId, f32)> = None;
    for (idx, slot) in ws.slots.iter().enumerate() {
        // 逻辑过期记录与墓碑一样不可见,不得参与判重(FC-LIFE-INV-009)。
        if ws.dead.get(idx) || slot.ns_id != probe.ns_id || !slot.is_live(probe.now) {
            continue;
        }
        let sim = score::cosine_sim(&rec.vector, &slot.vector);
        if sim >= probe.config.dedup_threshold && best.is_none_or(|(_, previous)| sim > previous) {
            best = Some((slot.rowid, sim));
        }
    }
    best
}

/// 单条写入:校验 → 去重 → 定位 `RowId` → 提交新版本。
pub(crate) fn insert_one(
    ws: &mut WriterState,
    config: &Config,
    ctx: InsertCtx,
) -> Result<InsertOutcome> {
    validate_insert(config, &ctx.rec)?;
    let duplicate = if matches!(config.dedup, Dedup::Off | Dedup::KeepBoth) {
        None
    } else {
        find_duplicate(
            ws,
            DedupProbe {
                config,
                ns_id: ctx.ns_id,
                now: ctx.now,
            },
            &ctx.rec,
        )
    };
    // `Replace` 语义要求生成新 RowId,即使新记录带同 key 也不能复用旧行。
    let force_new_rowid = duplicate.is_some() && matches!(config.dedup, Dedup::Replace);
    if let Some(outcome) = apply_dedup(ws, config, &ctx, duplicate)? {
        return Ok(outcome);
    }
    let rowid = match resolve_rowid(ws, config, &ctx, force_new_rowid)? {
        RowIdChoice::Use(rowid) => rowid,
        RowIdChoice::BatchDuplicate(existing) => {
            return Ok(InsertOutcome::Duplicate {
                existing,
                score: 0.0,
            });
        }
    };
    let seqno = ws.alloc_seqno();
    let slot_data = build_slot(SlotSpec {
        ns_id: ctx.ns_id,
        ns_path: ctx.ns_path,
        rowid,
        seqno,
        tx_ms: ctx.now,
        rec: ctx.rec,
    });
    ws.commit_version(rowid, slot_data)?;
    Ok(InsertOutcome::Inserted(rowid))
}

/// 命中重复时的处理;`Ok(Some(_))` 表示提前返回该结果。
fn apply_dedup(
    ws: &mut WriterState,
    config: &Config,
    ctx: &InsertCtx,
    duplicate: Option<(RowId, f32)>,
) -> Result<Option<InsertOutcome>> {
    let Some((existing, similarity)) = duplicate else {
        return Ok(None);
    };
    match &config.dedup {
        Dedup::Reject => Ok(Some(InsertOutcome::Duplicate {
            existing,
            score: similarity,
        })),
        Dedup::Replace => {
            let seqno = ws.alloc_seqno();
            ws.tombstone(existing, ctx.now, seqno)?;
            Ok(None)
        }
        Dedup::Merge(_) => merge_duplicate(ws, config, ctx, existing),
        Dedup::Off | Dedup::KeepBoth => Ok(None),
    }
}

/// `Dedup::Merge`:以旧记录为主体应用回调,返回合并后的新记录。
fn merge_duplicate(
    ws: &mut WriterState,
    config: &Config,
    ctx: &InsertCtx,
    existing: RowId,
) -> Result<Option<InsertOutcome>> {
    // 回调是 `Dedup::Merge` 自身携带的函数指针,从配置取回即可,不作为第 5 个参数透传。
    let Dedup::Merge(callback) = &config.dedup else {
        return Ok(None);
    };
    let existing_data = latest_live(ws, existing).ok_or(MnemeError::Inconsistent {
        reason: "去重命中但记录不可见",
    })?;
    let existing_ref = RecordRef::new(existing_data);
    // 预览版本须与 `ctx.rec` 独立:回调返回 `None` 时仍要用原记录走常规插入,故此处克隆。
    let temp = build_slot(SlotSpec {
        ns_id: ctx.ns_id,
        ns_path: Arc::clone(&ctx.ns_path),
        rowid: existing,
        seqno: SeqNo::new(0),
        tx_ms: ctx.now,
        rec: ctx.rec.clone(),
    });
    let new_ref = RecordRef::new(Arc::new(temp));
    let Some(merged) = callback(&existing_ref, &new_ref) else {
        return Ok(None);
    };
    validate_insert(config, &merged)?;
    let seqno = ws.alloc_seqno();
    let slot_data = build_slot(SlotSpec {
        ns_id: ctx.ns_id,
        ns_path: Arc::clone(&ctx.ns_path),
        rowid: existing,
        seqno,
        tx_ms: ctx.now,
        rec: merged,
    });
    ws.commit_version(existing, slot_data)?;
    Ok(Some(InsertOutcome::Merged(existing)))
}

/// `RowId` 选择结果。
enum RowIdChoice {
    /// 使用该 `RowId` 继续写入。
    Use(RowId),
    /// 批量写入下命中 `RejectDuplicate`,提前返回 `Duplicate`。
    BatchDuplicate(RowId),
}

/// 决定本次写入使用的 `RowId`(复用/新建/去重拒绝)。
fn resolve_rowid(
    ws: &mut WriterState,
    config: &Config,
    ctx: &InsertCtx,
    force_new_rowid: bool,
) -> Result<RowIdChoice> {
    if force_new_rowid {
        return Ok(RowIdChoice::Use(ws.alloc_rowid()));
    }
    let Some(key) = &ctx.rec.key else {
        return Ok(RowIdChoice::Use(ws.alloc_rowid()));
    };
    let existing = ws
        .key_index
        .get(&(ctx.ns_id, Key::new(key.as_str())))
        .copied();
    match (existing, config.insert_mode) {
        // 仅当同 key 存在**可见**记录(未墓碑且未逻辑过期)时才拒绝重复;墓碑/逻辑过期
        // 视为不存在并复用既有 `RowId`(与 `Dedup::Reject`/读路径同口径,FC-LIFE-INV-009)。
        (Some(existing), InsertMode::RejectDuplicate)
            if latest_live(ws, existing).is_some_and(|slot| slot.is_live(ctx.now)) =>
        {
            if ctx.in_batch {
                Ok(RowIdChoice::BatchDuplicate(existing))
            } else {
                Err(MnemeError::DuplicateKey(Key::new(key.as_str())))
            }
        }
        (Some(existing), _) => Ok(RowIdChoice::Use(existing)),
        (None, _) => Ok(RowIdChoice::Use(ws.alloc_rowid())),
    }
}
