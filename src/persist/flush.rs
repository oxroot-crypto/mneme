//! 全量快照 flush:把可变表写成新段(设计 04 §3.2 的 L2 兜底)。
//!
//! L2 尚无 compaction,`flush` 将整个 `WriterState` 物化为一个不可变段的
//! `vsec` + `msec` 两个文件;旧段在 MANIFEST 提交后进入 `trash/`。每条写入已先
//! 追加 WAL,故 flush 只是把已确认状态转成段文件、推进水位并重置 WAL。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::error::Result;
use crate::memory::config::Config;
use crate::memory::index::{IndexNode, VectorIndex};
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

/// 一次段编码的产物:vsec/msec/hidx 字节与内存索引(供 flush 安装到写状态)。
pub(crate) struct EncodedSegment {
    /// 向量段字节。
    pub(crate) vsec: Vec<u8>,
    /// 元数据段字节。
    pub(crate) msec: Vec<u8>,
    /// HNSW 图段字节(节点为空或未配置索引工厂时为 `None`)。
    pub(crate) hidx: Option<Vec<u8>>,
    /// 内存索引(供 flush 安装到写状态;与 `hidx` 对应)。
    pub(crate) index: Option<Arc<dyn VectorIndex>>,
    /// 段内入口槽位(无索引时为 0)。
    pub(crate) entry_slot: u32,
    /// 段内入口层级(无索引时为 0)。
    pub(crate) entry_level: u8,
}

/// 把一个写状态编码为 `vsec`/`msec`/`hidx` 与内存索引。
///
/// # Errors
/// 任一编解码、限额或索引构建失败时返回结构化错误。
pub(crate) fn build_segment(
    ws: &WriterState,
    config: &Config,
    created_unix_ms: i64,
) -> Result<EncodedSegment> {
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

    let built_index = build_index(ws, config);
    Ok(EncodedSegment {
        vsec: vsec_bytes,
        msec: msec_bytes,
        hidx: built_index.bytes,
        index: built_index.index,
        entry_slot: built_index.entry_slot,
        entry_level: built_index.entry_level,
    })
}

/// 一次索引构建的产物(hidx 字节、内存索引与入口)。
struct BuiltIndex {
    bytes: Option<Vec<u8>>,
    index: Option<Arc<dyn VectorIndex>>,
    entry_slot: u32,
    entry_level: u8,
}

/// 构建 HNSW 图并序列化为 hidx(无工厂或空表时各字段为空/零)。
fn build_index(ws: &WriterState, config: &Config) -> BuiltIndex {
    let Some(factory) = config.index_factory.as_ref() else {
        return BuiltIndex {
            bytes: None,
            index: None,
            entry_slot: 0,
            entry_level: 0,
        };
    };
    if ws.slots.is_empty() {
        return BuiltIndex {
            bytes: None,
            index: None,
            entry_slot: 0,
            entry_level: 0,
        };
    }
    let nodes: Vec<IndexNode> = ws
        .slots
        .iter()
        .map(|slot| IndexNode {
            rowid: slot.rowid,
            vector: Arc::clone(&slot.vector),
            norm_sq: slot.norm_sq,
        })
        .collect();
    let index = factory.build(&nodes, config.hnsw, config.metric);
    let entry = index.entry();
    let bytes = index.serialize();
    BuiltIndex {
        bytes: Some(bytes),
        index: Some(index),
        entry_slot: entry.0.get(),
        entry_level: entry.1,
    }
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
