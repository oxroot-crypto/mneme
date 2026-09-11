//! 全量快照 flush:把可变表写成新段(设计 04 §3.2 的 L2 兜底)。
//!
//! L2 尚无 compaction,`flush` 将整个 `WriterState` 物化为一个不可变段的
//! `vsec` + `msec` 两个文件;旧段在 MANIFEST 提交后进入 `trash/`。每条写入已先
//! 追加 WAL,故 flush 只是把已确认状态转成段文件、推进水位并重置 WAL。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::error::Result;
use crate::memory::analysis::ZONE_BLOCK_ROWS;
use crate::memory::config::Config;
use crate::memory::index::{IndexNode, VectorIndex};
use crate::memory::table::{SlotData, WriterState};
use crate::persist::edges::EdgeData;
use crate::persist::msec::{self, EntryData, FieldKind, MsecInput, NsStatRow, SlotMeta};
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
    let indexes = build_indexes(ws, config, built.slots.len())?;
    let msec_bytes = msec::encode(&MsecInput {
        slots: &built.slots,
        ns_stats: &ns_stats,
        delta: &[],
        relations: &relations_bytes,
        field_dict: &indexes.field_dict,
        zmap: &indexes.zmap,
        bloom: &indexes.bloom,
        inverted: &indexes.inverted,
    })?;

    let built_index = build_index(ws, config)?;
    Ok(EncodedSegment {
        vsec: vsec_bytes,
        msec: msec_bytes,
        hidx: built_index.bytes,
        index: built_index.index,
        entry_slot: built_index.entry_slot,
        entry_level: built_index.entry_level,
    })
}

/// 由写状态的加速结构构建 msec 四区(字段字典 / zone map / bloom / 倒排)。
///
/// 字段字典包含 zone 已索引的数值/时间字段与 `key`(bloom 字段),总数受
/// `Tuning.field_dict_max` 约束;超出的 metadata 字段查询期仍走行级求值。
///
/// # Errors
/// 倒排编码超 `u32` 长度或 postings 违背升序不变量时返回结构化错误。
fn build_indexes(ws: &WriterState, config: &Config, row_count: usize) -> Result<SegmentIndexBlobs> {
    let max_fields = (config.tuning.field_dict_max as usize).max(1);
    let mut fields: Vec<(Arc<str>, FieldKind)> = ws
        .zones
        .fields_iter()
        .map(|(name, kind)| (Arc::from(name), FieldKind::from_zone(kind)))
        .collect();
    fields.sort_by(|left, right| left.0.cmp(&right.0));
    // 给 `key`(bloom 字段)预留一个槽位;metadata 中的同名 `key` 先剔除,
    // 避免字段字典出现重名条目(行级求值里 `key` 是保留字段)。
    fields.retain(|(name, _)| name.as_ref() != "key");
    fields.truncate(max_fields.saturating_sub(1));
    fields.push((Arc::from("key"), FieldKind::Str));
    let key_field_id = u16::try_from(fields.len() - 1).map_err(|_| {
        crate::core::error::MnemeError::LimitExceeded {
            field: "field_dict",
            limit: u16::MAX as usize,
            got: fields.len(),
        }
    })?;

    let block_count = row_count.div_ceil(ZONE_BLOCK_ROWS);
    Ok(SegmentIndexBlobs {
        field_dict: msec::encode_field_dict(&fields),
        zmap: msec::encode_zmap(&ws.zones, &fields, block_count),
        bloom: msec::encode_bloom(&ws.key_bloom, key_field_id),
        inverted: msec::encode_inverted(&ws.inv)?,
    })
}

/// 一次段索引编码的产物(四个区字节)。
struct SegmentIndexBlobs {
    field_dict: Vec<u8>,
    zmap: Vec<u8>,
    bloom: Vec<u8>,
    inverted: Vec<u8>,
}

/// 一次索引构建的产物(hidx 字节、内存索引与入口)。
struct BuiltIndex {
    bytes: Option<Vec<u8>>,
    index: Option<Arc<dyn VectorIndex>>,
    entry_slot: u32,
    entry_level: u8,
}

/// 构建 HNSW 图并序列化为 hidx(无工厂或空表时各字段为空/零)。
///
/// # Errors
/// 图序列化失败(hidx 长度字段超出格式上限)时返回结构化错误。
fn build_index(ws: &WriterState, config: &Config) -> Result<BuiltIndex> {
    let Some(factory) = config.index_factory.as_ref() else {
        return Ok(BuiltIndex {
            bytes: None,
            index: None,
            entry_slot: 0,
            entry_level: 0,
        });
    };
    if ws.slots.is_empty() {
        return Ok(BuiltIndex {
            bytes: None,
            index: None,
            entry_slot: 0,
            entry_level: 0,
        });
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
    let bytes = index.serialize()?;
    Ok(BuiltIndex {
        bytes: Some(bytes),
        index: Some(index),
        entry_slot: entry.0.get(),
        entry_level: entry.1,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::core::meta::json;
    use crate::core::metric::Metric;
    use crate::core::options::{
        CompactionPolicy, Compression, Dimension, FsyncPolicy, HnswParams, InsertMode, Limits,
        RelationIndex, SystemClock, Tuning, VectorFormat,
    };
    use crate::core::types::{Key, NsId, RowId, SeqNo};
    use crate::memory::dedup::Dedup;
    use crate::memory::table::SlotData;

    /// 构造仅 `tuning.field_dict_max` 等字段有意义的最小运行配置。
    fn test_config() -> Config {
        Config {
            dimension: Dimension::new(2).expect("dimension"),
            metric: Metric::Cosine,
            fsync: FsyncPolicy::default(),
            insert_mode: InsertMode::default(),
            dedup: Dedup::default(),
            dedup_threshold: 0.9,
            quantization: VectorFormat::default(),
            hnsw: HnswParams::default(),
            index_factory: None,
            compaction: CompactionPolicy::default(),
            retention: None,
            retain_interval: None,
            access_flush_interval: Duration::from_secs(60),
            compression: Compression::default(),
            relation_index: RelationIndex::default(),
            parallelism: 1,
            tuning: Tuning::default(),
            limits: Limits::default(),
            clock: Arc::new(SystemClock),
            read_only: false,
            verify_on_open: false,
            fail_fast_on_corruption: false,
        }
    }

    /// 构造一条 metadata 里带数值 `key` 的槽位(仅字段字典去重测试用)。
    fn slot_with_numeric_key() -> Arc<SlotData> {
        Arc::new(SlotData {
            rowid: RowId::new(0),
            ns_id: NsId::new(1),
            ns_path: Arc::from("n"),
            seqno: SeqNo::new(1),
            key: Some(Key::new("k0")),
            vector: Arc::from(vec![0.0_f32, 1.0].into_boxed_slice()),
            norm_sq: 1.0,
            text: None,
            text_hash: None,
            meta: json!({"key": 7}),
            created_at: 1_000,
            expires_at: None,
            importance: 0.5,
            confidence: 1.0,
            valid_from: 1_000,
            valid_to: None,
            provenance: None,
            tx_ms: 1_000,
            deleted: false,
        })
    }

    /// FC-PERSIST-POST-008(字段字典 `key` 去重:metadata 数值 `key` 不得与 bloom 保留字段重名)
    #[test]
    fn field_dict_keeps_single_key_field() {
        let mut ws = WriterState::new();
        let slot = slot_with_numeric_key();
        Arc::make_mut(&mut ws.slots).push(Arc::clone(&slot));
        Arc::make_mut(&mut ws.zones).observe(0, &slot);
        let blobs = build_indexes(&ws, &test_config(), 1).expect("build_indexes");
        let defs = msec::decode_field_dict(&blobs.field_dict).expect("field_dict");
        let key_fields: Vec<_> = defs
            .iter()
            .filter(|field| field.name.as_ref() == "key")
            .collect();
        assert_eq!(key_fields.len(), 1, "字段字典中 `key` 必须唯一");
        assert_eq!(
            key_fields[0].kind,
            msec::FieldKind::Str,
            "唯一 `key` 必须是 bloom 用的字符串保留字段"
        );
        let blooms = msec::decode_bloom(&blobs.bloom).expect("bloom");
        assert_eq!(blooms.len(), 1);
        assert_eq!(
            blooms[0].0, key_fields[0].id,
            "bloom 字段编号必须指向唯一的 `key`"
        );
    }
}
