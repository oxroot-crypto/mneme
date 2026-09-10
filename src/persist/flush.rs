//! 全量快照 flush:把可变表写成新段(设计 04 §3.2 的 L2 兜底)。
//!
//! L2 尚无 compaction,`flush` 将整个 `WriterState` 物化为一个不可变段的
//! `vsec` + `msec` 两个文件;旧段在 MANIFEST 提交后进入 `trash/`。每条写入已先
//! 追加 WAL,故 flush 只是把已确认状态转成段文件、推进水位并重置 WAL。

use std::collections::HashMap;

use crate::core::error::Result;
use crate::memory::config::Config;
use crate::memory::table::{SlotData, WriterState};
use crate::persist::edges::EdgeData;
use crate::persist::msec::{self, EntryData, MsecInput, NsStatRow, SlotMeta};
use crate::persist::vsec::{self, VsecInput};

/// 段内槽位及其对应的向量/范数/删除位(向量借用自写状态)。
struct SegmentSlots<'a> {
    slots: Vec<SlotMeta>,
    vectors: Vec<&'a [f32]>,
    norms: Vec<f32>,
    dead: Vec<bool>,
}

/// 把一个写状态编码为 `(vsec 字节, msec 字节)`。
///
/// # Errors
/// 任一编解码或长度限额失败时返回结构化错误。
pub(crate) fn build_segment(
    ws: &WriterState,
    config: &Config,
    created_unix_ms: i64,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let built = build_slots(ws);
    let vsec_bytes = vsec::encode(&VsecInput {
        dimension: config.dimension.get(),
        metric: config.metric,
        created_unix_ms,
        vectors: &built.vectors,
        norms: &built.norms,
        dead: &built.dead,
    })?;

    let ns_stats = build_ns_stats(ws, config);
    let relations = build_relations(ws);
    let relations_bytes = crate::persist::edges::encode(&relations, false);
    let msec_bytes = msec::encode(&MsecInput {
        slots: &built.slots,
        ns_stats: &ns_stats,
        delta: &[],
        relations: &relations_bytes,
    })?;
    Ok((vsec_bytes, msec_bytes))
}

/// 由写状态构造段内槽位与 vsec 输入列。
fn build_slots(ws: &WriterState) -> SegmentSlots<'_> {
    let count = ws.slots.len();
    let mut built = SegmentSlots {
        slots: Vec::with_capacity(count),
        vectors: Vec::with_capacity(count),
        norms: Vec::with_capacity(count),
        dead: Vec::with_capacity(count),
    };
    for (index, slot) in ws.slots.iter().enumerate() {
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
    let access = ws
        .access
        .get(&slot.rowid)
        .map(|stat| (stat.last_access_ms, stat.access_count));
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
        access,
        valid_time: Some((slot.valid_from, slot.valid_to)),
        confidence: Some(slot.confidence),
        provenance: slot.provenance.clone(),
    })
}

/// 统计各命名空间的活行数与文本总长(段内剪枝/BM25 用)。
fn build_ns_stats(ws: &WriterState, config: &Config) -> Vec<NsStatRow> {
    let now = config.clock.now_unix_ms();
    let mut stats: HashMap<u32, (u64, u64)> = HashMap::new();
    for (index, slot) in ws.slots.iter().enumerate() {
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
