//! 增量段物化:把未落盘槽位写成新段(设计 04 §3.2、07 §4)。
//!
//! `flush` 只物化 `slot_segment == None` 的槽位与自上次 flush 的访问/关系 delta;
//! 旧段保持活跃、不改写、不入 `trash/`。`watermark` 推进后 WAL 方可 Checkpoint。
//! 每条写入已先追加 WAL,故 flush 只是把已确认状态转成段文件。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::bitset::BitSet;
use crate::core::error::Result;
use crate::core::types::SlotId;
use crate::memory::analysis::{
    BLOOM_INITIAL_CAPACITY, BloomSet, InvertedIndex, ZONE_BLOCK_ROWS, ZoneIndex,
};
use crate::memory::config::Config;
use crate::memory::index::{IndexNode, VectorIndex};
use crate::memory::table::{SlotData, WriterState};
use crate::persist::edges::EdgeData;
use crate::persist::msec::{
    self, DeltaEntry, EntryData, FieldKind, MsecInput, NsStatRow, SlotMeta,
};
use crate::persist::vsec::{self, VsecInput};

/// 一次段物化的输入:要写入的全局槽位(升序)与跨段 delta。
pub(crate) struct SegmentBuildInput<'a> {
    /// 要物化的全局槽位下标(升序);空 = 纯 delta 段。
    pub(crate) slots: &'a [usize],
    /// 跨段覆盖条目(访问/关系);`full_relations` 为真时忽略关系部分。
    pub(crate) delta: &'a [DeltaEntry],
    /// 是否全量重写关系表(首个段或 compaction);否则关系变更走 delta。
    pub(crate) full_relations: bool,
}

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

/// 把写状态中指定槽位编码为 `vsec`/`msec`/`hidx` 与内存索引。
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
    let vsec_bytes = vsec::encode(&VsecInput {
        dimension: config.dimension.get(),
        metric: config.metric,
        created_unix_ms,
        vectors: &built.vectors,
        norms: &built.norms,
        dead: &built.dead,
    })?;

    let ns_stats = build_ns_stats(ws, config, input.slots);
    let relations = if input.full_relations {
        build_relations(ws)
    } else {
        Vec::new()
    };
    let write_reverse = config.relation_index == crate::core::options::RelationIndex::Both;
    let relations_bytes = crate::persist::edges::encode(&relations, write_reverse)?;
    let indexes = build_indexes(ws, config, input.slots)?;
    let delta_bytes = msec::encode_delta(input.delta)?;
    let msec_bytes = msec::encode(&MsecInput {
        slots: &built.slots,
        ns_stats: &ns_stats,
        delta: &delta_bytes,
        relations: &relations_bytes,
        field_dict: &indexes.field_dict,
        zmap: &indexes.zmap,
        bloom: &indexes.bloom,
        inverted: &indexes.inverted,
    })?;

    let built_index = build_index(ws, config, input.slots)?;
    Ok(EncodedSegment {
        vsec: vsec_bytes,
        msec: msec_bytes,
        hidx: built_index.bytes,
        index: built_index.index,
        entry_slot: built_index.entry_slot,
        entry_level: built_index.entry_level,
    })
}

/// 由写状态构造本次段物化需要的 delta 条目(访问计数 + 关系边净变更)。
///
/// `full_relations` 为真(首段/compaction)时关系表全量重写,不再产出关系 delta;
/// `included` 中已有最新版本的 RowId,其访问统计随记录体列落盘,无需 Access 条目。
pub(crate) fn build_delta(
    ws: &WriterState,
    included: &[usize],
    now_ms: i64,
    full_relations: bool,
) -> Vec<DeltaEntry> {
    let mut in_segment = BitSet::default();
    for &idx in included {
        in_segment.set(idx);
    }
    let mut entries = Vec::new();
    for (&rowid, &access_delta) in ws.access_dirty.iter() {
        // 最新版本随本段落盘时,记录体的 access 列已携带最新统计。
        if let Some(latest) = ws.latest.get(&rowid)
            && in_segment.get(latest.get() as usize)
        {
            continue;
        }
        let stat = ws.access.get(&rowid).copied().unwrap_or_default();
        let ns_id = ws
            .latest
            .get(&rowid)
            .map_or(0, |slot| ws.slots[slot.get() as usize].ns_id.get());
        entries.push(DeltaEntry::Access {
            seqno: ws.seqno.get(),
            tx_ms: now_ms,
            ns_id,
            rowid: rowid.get(),
            last_access_ms: stat.last_access_ms,
            access_delta,
            importance_delta: 0.0,
        });
    }
    if !full_relations {
        for &(from, to, kind) in ws.edge_dirty.iter() {
            let edge = ws.out_edges.get(&from).and_then(|edges| {
                edges
                    .iter()
                    .find(|edge| edge.to == to && edge.kind.0 == kind)
            });
            let ns_id = ws
                .latest
                .get(&from)
                .map_or(0, |slot| ws.slots[slot.get() as usize].ns_id.get());
            entries.push(match edge {
                Some(edge) => DeltaEntry::Relate {
                    seqno: ws.seqno.get(),
                    tx_ms: now_ms,
                    ns_id,
                    from: from.get(),
                    to: to.get(),
                    kind,
                    weight: edge.weight,
                    meta: edge.metadata.clone(),
                },
                None => DeltaEntry::Unrelate {
                    seqno: ws.seqno.get(),
                    tx_ms: now_ms,
                    ns_id,
                    from: from.get(),
                    to: to.get(),
                    kind,
                },
            });
        }
    }
    entries
}

/// 由本次物化的槽位构建局部 zone map / bloom / 倒排在段内局部编号上编码。
///
/// 全局加速结构(`ws.zones`/`ws.inv`/`ws.key_bloom`)覆盖全部槽位,块编号与
/// 段内块不一致,不可直接编码;增量段按局部槽位重建这三区(代价正比于本段行数)。
///
/// # Errors
/// 倒排编码超 `u32` 长度或 postings 违背升序不变量时返回结构化错误。
fn build_indexes(
    ws: &WriterState,
    config: &Config,
    included: &[usize],
) -> Result<SegmentIndexBlobs> {
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

    let block_count = included.len().div_ceil(ZONE_BLOCK_ROWS);
    let mut local_zones = ZoneIndex::new(max_fields);
    let mut local_bloom = BloomSet::new(BLOOM_INITIAL_CAPACITY, config.tuning.bloom_fpp);
    let mut local_inv = InvertedIndex::default();
    let mut min_expires = vec![i64::MAX; block_count];
    for (local, &idx) in included.iter().enumerate() {
        let slot_data = &ws.slots[idx];
        if slot_data.deleted {
            continue;
        }
        local_zones.observe(local, slot_data);
        if let Some(text) = &slot_data.text {
            local_inv.insert_text(
                SlotId::new(local as u32),
                slot_data.ns_id,
                text,
                ws.stopwords_enabled,
            );
        }
        if let Some(key) = &slot_data.key {
            local_bloom.insert(key.as_str());
        }
        // TTL 块级剪枝:块内 min(expires_at),无 TTL 行记 +∞。
        if let Some(expires) = slot_data.expires_at {
            let block = local / ZONE_BLOCK_ROWS;
            min_expires[block] = min_expires[block].min(expires);
        }
    }
    let mut zmap = msec::encode_zmap(&local_zones, &fields, block_count);
    zmap.extend_from_slice(&msec::encode_ttl_map(&min_expires));

    Ok(SegmentIndexBlobs {
        field_dict: msec::encode_field_dict(&fields),
        zmap,
        bloom: msec::encode_bloom(&local_bloom, key_field_id),
        inverted: msec::encode_inverted(&local_inv)?,
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

/// 构建 HNSW 图并序列化为 hidx(无工厂或空段时各字段为空/零)。
///
/// # Errors
/// 图序列化失败(hidx 长度字段超出格式上限)时返回结构化错误。
fn build_index(ws: &WriterState, config: &Config, included: &[usize]) -> Result<BuiltIndex> {
    let Some(factory) = config.index_factory.as_ref() else {
        return Ok(BuiltIndex {
            bytes: None,
            index: None,
            entry_slot: 0,
            entry_level: 0,
        });
    };
    if included.is_empty() {
        return Ok(BuiltIndex {
            bytes: None,
            index: None,
            entry_slot: 0,
            entry_level: 0,
        });
    }
    let nodes: Vec<IndexNode> = included
        .iter()
        .map(|&idx| {
            let slot = &ws.slots[idx];
            IndexNode {
                rowid: slot.rowid,
                vector: Arc::clone(&slot.vector),
                norm_sq: slot.norm_sq,
            }
        })
        .collect();
    let slot_of: Vec<SlotId> = included
        .iter()
        .map(|&idx| {
            // 槽位下标 ≤ u32::MAX(FC-MEM-INV-004),转换可证明不会失败。
            SlotId::new(u32::try_from(idx).expect("槽位下标必可转入 u32(FC-MEM-INV-004)"))
        })
        .collect();
    let index = factory.build(&nodes, &slot_of, config.hnsw, config.metric);
    let entry = index.entry();
    let bytes = index.serialize()?;
    Ok(BuiltIndex {
        bytes: Some(bytes),
        index: Some(index),
        entry_slot: entry.0.get(),
        entry_level: entry.1,
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
        let blobs = build_indexes(&ws, &test_config(), &[0]).expect("build_indexes");
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
