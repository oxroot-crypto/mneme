//! 段构建个总编排同 `vsec`/`msec` 编码、段内槽位/统计区个构造。

use std::collections::HashMap;

use crate::core::error::Result;
use crate::core::options::VectorFormat;
use crate::memory::config::Config;
use crate::memory::table::{SlotData, WriterState};
use crate::persist::edges::EdgeData;
use crate::persist::msec::{self, EntryData, MsecInput, NsStatRow, SlotMeta};
use crate::persist::vsec::{self, VsecInput};

use super::index::{BuildIndexInput, BuiltIndex, build_index, build_indexes};
use super::quant::{FinalizeQuantInput, finalize_quant, plan_quant};
use super::types::{EncodedSegment, QuantDecision, SegmentBuildInput, SegmentSlots};

/// 把写状态中指定槽位编码为 `vsec`/`msec`/`hidx` 与内存索引。
///
/// 量化开启时先建 f32 图再按当前配置生成副本,并以抽样一致率决定是否启用
/// (不达标回退 f32,`FC-QUANT-INV-013`);图结构始终由 f32 构建,副本只服务
/// 查询期粗排(设计 08 §落地状态)。
///
/// # Errors
/// 任一编解码、限额或索引构建失败时返回结构化错误。
pub(crate) fn build_segment(
    ws: &WriterState,
    config: &Config,
    created_unix_ms: i64,
    input: &SegmentBuildInput<'_>,
) -> Result<EncodedSegment> {
    let built = build_slots(ws, input.slots);
    let planned = plan_quant(config, &built.vectors)?;
    let mut built_index = build_index(BuildIndexInput {
        ws,
        config,
        included: input.slots,
        quant: planned.clone(),
        parallelism: input.parallelism,
    })?;
    let decision = finalize_quant(FinalizeQuantInput {
        config,
        ws,
        built: &built,
        included: input.slots,
        index: &built_index.index,
        planned,
    })?;
    if decision.fallback {
        built_index = build_index(BuildIndexInput {
            ws,
            config,
            included: input.slots,
            quant: None,
            parallelism: input.parallelism,
        })?;
    }
    encode_segment(EncodeSegmentInput {
        config,
        created_unix_ms,
        ws,
        segment: input,
        built: &built,
        decision,
        built_index,
    })
}

/// [`encode_segment`] 的输入参数。
struct EncodeSegmentInput<'a> {
    /// 库配置(维度/度量/压缩口径)。
    config: &'a Config,
    /// 段创建时刻(Unix 毫秒)。
    created_unix_ms: i64,
    /// 写状态(msec 记录体与访问统计来源)。
    ws: &'a WriterState,
    /// 段构建输入(槽位/delta/关系标志/并行度)。
    segment: &'a SegmentBuildInput<'a>,
    /// 段内槽位与向量列。
    built: &'a SegmentSlots<'a>,
    /// 量化建段决策(副本与召回估计)。
    decision: QuantDecision,
    /// 已构建的索引产物(hidx 字节与内存索引)。
    built_index: BuiltIndex,
}

/// 编码 vsec/msec 并汇总为 [`EncodedSegment`]([`build_segment`] 的后半段)。
fn encode_segment(input: EncodeSegmentInput<'_>) -> Result<EncodedSegment> {
    let vsec_bytes = encode_vsec(
        input.config,
        input.created_unix_ms,
        input.built,
        &input.decision,
    )?;
    let msec_bytes = encode_msec(input.config, input.ws, input.segment, input.built)?;
    Ok(EncodedSegment {
        vsec: vsec_bytes,
        msec: msec_bytes,
        hidx: input.built_index.bytes,
        index: input.built_index.index,
        entry_slot: input.built_index.entry_slot,
        entry_level: input.built_index.entry_level,
        quant: input
            .decision
            .copy
            .as_ref()
            .map_or(VectorFormat::F32, |copy| copy.format),
        recall_est: input.decision.recall_est,
    })
}

/// 编码 vsec:量化副本参数/码流与 f32 原向量同源写出。
fn encode_vsec(
    config: &Config,
    created_unix_ms: i64,
    built: &SegmentSlots<'_>,
    decision: &QuantDecision,
) -> Result<Vec<u8>> {
    let quant = decision
        .copy
        .as_ref()
        .map_or(VectorFormat::F32, |copy| copy.format);
    let quant_params: &[f32] = decision
        .copy
        .as_ref()
        .map_or(&[][..], |copy| copy.params.as_slice());
    let code_refs: Vec<&[u8]> = decision
        .copy
        .as_ref()
        .map(|copy| copy.rows.iter().map(AsRef::as_ref).collect())
        .unwrap_or_default();
    vsec::encode(&VsecInput {
        dimension: config.dimension.get(),
        metric: config.metric,
        created_unix_ms,
        vectors: &built.vectors,
        norms: &built.norms,
        dead: &built.dead,
        quant,
        quant_params,
        quant_codes: &code_refs,
    })
}

/// 编码 msec:命名空间统计、关系区、轻量索引四区与 delta 区。
fn encode_msec(
    config: &Config,
    ws: &WriterState,
    input: &SegmentBuildInput<'_>,
    built: &SegmentSlots<'_>,
) -> Result<Vec<u8>> {
    let ns_stats = build_ns_stats(ws, config, input.slots);
    let relations = if input.full_relations {
        build_relations(ws)
    } else {
        Vec::new()
    };
    let write_reverse = config.relation_index == crate::core::options::RelationIndex::Both;
    let relations_bytes =
        crate::persist::edges::encode(&relations, write_reverse, input.full_relations)?;
    let indexes = build_indexes(ws, config, input.slots)?;
    let delta_bytes = msec::encode_delta(input.delta)?;
    msec::encode(&MsecInput {
        slots: &built.slots,
        ns_stats: &ns_stats,
        delta: &delta_bytes,
        relations: &relations_bytes,
        field_dict: &indexes.field_dict,
        zmap: &indexes.zmap,
        bloom: &indexes.bloom,
        inverted: &indexes.inverted,
        compression: config.compression,
    })
}

/// 由写状态与指定槽位构造段内槽位与 vsec 输入列(向量借用自 `ws`)。
fn build_slots<'a>(ws: &'a WriterState, included: &[usize]) -> SegmentSlots<'a> {
    let mut built = SegmentSlots {
        slots: Vec::with_capacity(included.len()),
        vectors: Vec::with_capacity(included.len()),
        norms: Vec::with_capacity(included.len()),
        dead: Vec::with_capacity(included.len()),
    };
    for &index in included {
        let slot = &ws.slots[index];
        built.dead.push(ws.dead.get(index) || slot.deleted);
        built.slots.push(SlotMeta {
            rowid: slot.rowid,
            seqno: slot.seqno,
            tx_ms: slot.tx_ms,
            body: entry_body(slot, ws),
        });
        built.vectors.push(slot.vector.as_ref());
        built.norms.push(slot.norm_sq);
    }
    built
}

/// 由槽位构造 msec 记录体;墓碑返回 `None`。
fn entry_body(slot: &SlotData, ws: &WriterState) -> Option<EntryData> {
    if slot.deleted {
        return None;
    }
    // 访问统计**始终**写出(缺省 0):版本行的 `access` 列是写入时刻的累计快照,
    // 缺失会让恢复按"未携带"跳过覆盖,把更旧版本的值留在表里,delta 再累加即
    // 重复计数(FC-PERSIST-POST-010)。写 0 明确表示"该版本时点为 0 次"。
    let stat = ws.access.get(&slot.rowid).copied().unwrap_or_default();
    Some(EntryData {
        rowid: slot.rowid,
        seqno: slot.seqno,
        ns_id: slot.ns_id,
        key: slot.key.clone(),
        text: slot.text.clone(),
        meta: slot.meta.clone(),
        created_at_ms: slot.created_at,
        expires_at_ms: slot.expires_at,
        importance: Some(slot.importance),
        access: Some((stat.last_access_ms, stat.access_count)),
        valid_time: Some((slot.valid_from, slot.valid_to)),
        confidence: Some(slot.confidence),
        provenance: slot.provenance.clone(),
    })
}

/// 统计各命名空间的活行数与文本**字节**总长(msec `ns_stats` 区;查询期不消费,
/// BM25 的长度口径以倒排 doc 区的词数为准,见 `FC-QUERY-POST-003`)。
fn build_ns_stats(ws: &WriterState, config: &Config, included: &[usize]) -> Vec<NsStatRow> {
    let now = config.clock.now_unix_ms();
    let mut stats: HashMap<u32, (u64, u64)> = HashMap::new();
    for &index in included {
        let slot = &ws.slots[index];
        if ws.dead.get(index) || !slot.is_live(now) {
            continue;
        }
        let entry = stats.entry(slot.ns_id.get()).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += slot.text.as_ref().map_or(0, |text| text.len() as u64);
    }
    let mut rows: Vec<NsStatRow> = stats
        .into_iter()
        .map(|(ns_id, (doc_count, total_doc_len))| NsStatRow {
            ns_id,
            doc_count,
            total_doc_len,
        })
        .collect();
    rows.sort_by_key(|row| row.ns_id);
    rows
}

/// 收集出边为可编码的边表。
fn build_relations(ws: &WriterState) -> Vec<EdgeData> {
    let mut edges = Vec::new();
    for (from, bucket) in ws.out_edges.iter() {
        for edge in bucket {
            edges.push(EdgeData {
                from: from.get(),
                to: edge.to.get(),
                kind: edge.kind.0,
                weight: edge.weight,
                meta: edge.metadata.clone(),
            });
        }
    }
    edges
}
