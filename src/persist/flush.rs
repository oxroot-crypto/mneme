//! 增量段物化:把未落盘槽位写成新段(设计 04 §3.2、07 §4)。
//!
//! `flush` 只物化 `slot_segment == None` 的槽位与自上次 flush 的访问/关系 delta;
//! 旧段保持活跃、不改写、不入 `trash/`。`watermark` 推进后 WAL 方可 Checkpoint。
//! 每条写入已先追加 WAL,故 flush 只是把已确认状态转成段文件。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::bitset::BitSet;
use crate::core::error::{MnemeError, Result};
use crate::core::heap::TopK;
use crate::core::metric::Metric;
use crate::core::options::VectorFormat;
use crate::core::types::SlotId;
use crate::memory::analysis::{
    BLOOM_INITIAL_CAPACITY, BloomSet, InvertedIndex, ZONE_BLOCK_ROWS, ZoneIndex,
};
use crate::memory::config::Config;
use crate::memory::index::{IndexNode, IndexSearch, QuantCopy, VectorIndex};
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
    /// 本段 HNSW 批内建图并行度(`0` = 可用核数)。
    ///
    /// 多块 flush 时块间已并行,内层应传 `1` 避免嵌套过度订阅;
    /// 单块/compaction 逐段构建时传库配置并行度。
    pub(crate) parallelism: usize,
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
    /// 段实际生效的量化格式(`F32` = 无副本或建段抽样已回退)。
    pub(crate) quant: VectorFormat,
    /// 建段抽样召回一致率估计(`None` = 无副本)。
    pub(crate) recall_est: Option<f32>,
}

/// 量化建段决策:保留副本 / 回退 / 召回估计。
struct QuantDecision {
    /// 保留的副本(`None` = 无需量化或抽样不达标已回退)。
    copy: Option<QuantCopy>,
    /// 抽样一致率估计(仅保留副本时非空)。
    recall_est: Option<f32>,
    /// 是否因抽样不达标回退(调用方需重建无副本索引)。
    fallback: bool,
}

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
    let mut built_index = build_index(ws, config, input.slots, planned.clone(), input.parallelism)?;
    let decision = finalize_quant(config, ws, &built, input.slots, &built_index.index, planned)?;
    if decision.fallback {
        built_index = build_index(ws, config, input.slots, None, input.parallelism)?;
    }
    encode_segment(
        config,
        created_unix_ms,
        ws,
        input,
        &built,
        decision,
        built_index,
    )
}

/// 编码 vsec/msec 并汇总为 [`EncodedSegment`]([`build_segment`] 的后半段)。
fn encode_segment(
    config: &Config,
    created_unix_ms: i64,
    ws: &WriterState,
    input: &SegmentBuildInput<'_>,
    built: &SegmentSlots<'_>,
    decision: QuantDecision,
    built_index: BuiltIndex,
) -> Result<EncodedSegment> {
    let vsec_bytes = encode_vsec(config, created_unix_ms, built, &decision)?;
    let msec_bytes = encode_msec(config, ws, input, built)?;
    Ok(EncodedSegment {
        vsec: vsec_bytes,
        msec: msec_bytes,
        hidx: built_index.bytes,
        index: built_index.index,
        entry_slot: built_index.entry_slot,
        entry_level: built_index.entry_level,
        quant: decision
            .copy
            .as_ref()
            .map_or(VectorFormat::F32, |copy| copy.format),
        recall_est: decision.recall_est,
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

/// 按当前配置为段内向量生成量化副本;i8 逐维统计参数,`f32`/空段返回 `None`。
fn plan_quant(config: &Config, vectors: &[&[f32]]) -> Result<Option<QuantCopy>> {
    let format = config.quantization;
    if format == VectorFormat::F32 || vectors.is_empty() {
        return Ok(None);
    }
    let dimension = config.dimension.get() as usize;
    match format {
        VectorFormat::F32 => Ok(None),
        VectorFormat::I8Rescored => Ok(Some(plan_i8_copy(vectors, dimension)?)),
        VectorFormat::F16 => Ok(Some(plan_f16_copy(vectors, dimension)?)),
    }
}

/// i8 量化副本:逐维统计缩放参数,再编码全段行。
fn plan_i8_copy(vectors: &[&[f32]], dimension: usize) -> Result<QuantCopy> {
    let params = crate::quant::scalar_i8::build_params(vectors, dimension)?;
    let table = params.table();
    let mut codes = Vec::with_capacity(vectors.len() * dimension);
    for vector in vectors {
        crate::quant::scalar_i8::encode_row_into(&mut codes, vector, &params);
    }
    let rows = crate::memory::lazy::LazyRows::from_owned(codes, dimension, vectors.len()).ok_or(
        MnemeError::Inconsistent {
            reason: "i8 量化副本行区长度不符",
        },
    )?;
    Ok(QuantCopy {
        format: VectorFormat::I8Rescored,
        params: table,
        rows,
    })
}

/// f16 量化副本(需 feature `quant-f16`;未开启显式 `Unsupported`)。
fn plan_f16_copy(vectors: &[&[f32]], dimension: usize) -> Result<QuantCopy> {
    #[cfg(feature = "quant-f16")]
    {
        let dimension_bytes = dimension * 2;
        let mut codes = Vec::with_capacity(vectors.len() * dimension_bytes);
        for vector in vectors {
            crate::quant::f16::encode_row_into(&mut codes, vector);
        }
        let rows = crate::memory::lazy::LazyRows::from_owned(codes, dimension_bytes, vectors.len())
            .ok_or(MnemeError::Inconsistent {
                reason: "f16 量化副本行区长度不符",
            })?;
        Ok(QuantCopy {
            format: VectorFormat::F16,
            params: Vec::new(),
            rows,
        })
    }
    #[cfg(not(feature = "quant-f16"))]
    {
        let _ = (vectors, dimension);
        Err(MnemeError::Unsupported {
            feature: "quant-f16",
        })
    }
}

/// 抽样评估量化召回:一致率达标保留副本,不达标回退 f32(I13)。
fn finalize_quant(
    config: &Config,
    ws: &WriterState,
    built: &SegmentSlots<'_>,
    included: &[usize],
    index: &Option<Arc<dyn VectorIndex>>,
    planned: Option<QuantCopy>,
) -> Result<QuantDecision> {
    let Some(copy) = planned else {
        return Ok(QuantDecision {
            copy: None,
            recall_est: None,
            fallback: false,
        });
    };
    let Some(index) = index else {
        return Err(MnemeError::Inconsistent {
            reason: "量化建段缺少索引,无法抽样评估",
        });
    };
    let estimate = estimate_recall(config, ws, built, included, index.as_ref())?;
    if estimate < config.tuning.quant_recall_floor {
        return Ok(QuantDecision {
            copy: None,
            recall_est: None,
            fallback: true,
        });
    }
    Ok(QuantDecision {
        copy: Some(copy),
        recall_est: Some(estimate),
        fallback: false,
    })
}

/// 建段抽样召回估计:同一 f32 图下,量化粗排 + f32 精排 top-k 与纯 f32 top-k
/// 的平均一致率(等距抽样 `RECALL_SAMPLE_QUERIES` 条自查询)。
fn estimate_recall(
    config: &Config,
    ws: &WriterState,
    built: &SegmentSlots<'_>,
    included: &[usize],
    index: &dyn VectorIndex,
) -> Result<f32> {
    let node_count = index.node_count();
    if node_count == 0 {
        return Ok(1.0);
    }
    let mut alive = BitSet::default();
    for &slot in included {
        alive.set(slot);
    }
    // 注:`alive` 按本次物化的全部槽位置位(含墓碑/死行),与运行期「可见行」
    // 口径不同;参考与候选两侧同口径,故一致率决策不受影响。
    let k = crate::quant::rescore::RECALL_TOP_K.min(node_count);
    let candidate_cap =
        crate::quant::rescore::coarse_candidates(k, config.tuning.rescore_oversample)
            .min(node_count);
    let indices = crate::quant::rescore::sample_indices(node_count);
    let mut total = 0.0_f32;
    for &node in &indices {
        // 建图节点顺序与 `included` 一致(node id = 段内下标)。
        let query = built.vectors[node];
        let query_norm = if config.metric.needs_norm() {
            crate::memory::search::norm_sq(query)
        } else {
            0.0
        };
        let reference = search_payloads(index, query, query_norm, &alive, k, config, false);
        let coarse = search_payloads(
            index,
            query,
            query_norm,
            &alive,
            candidate_cap,
            config,
            true,
        );
        let candidate = rescore_payloads(ws, config.metric, query, query_norm, &coarse, k);
        let reference_ids: Vec<u64> = reference.iter().map(|(rowid, _)| rowid.get()).collect();
        let candidate_ids: Vec<u64> = candidate.iter().map(|(rowid, _)| rowid.get()).collect();
        total += crate::quant::rescore::agreement(&reference_ids, &candidate_ids);
    }
    Ok(total / indices.len() as f32)
}

/// 在段索引上搜索并返回排序后的 `(RowId, SlotId)` 列表(`use_quant` 控粗排口径)。
fn search_payloads(
    index: &dyn VectorIndex,
    query: &[f32],
    query_norm: f32,
    alive: &BitSet,
    k: usize,
    config: &Config,
    use_quant: bool,
) -> Vec<(crate::core::types::RowId, SlotId)> {
    let top = index.search(&IndexSearch {
        query,
        query_norm,
        ef: config.hnsw.ef_search as usize,
        k,
        alive,
        filter: None,
        post_threshold: config.tuning.filter_post_threshold,
        brute_threshold: config.tuning.filter_brute_threshold,
        use_quant,
        bias: None,
    });
    top.into_sorted_vec()
}

/// 对粗排候选按 f32 原向量精排,取 top-k(两阶段第二阶段)。
fn rescore_payloads(
    ws: &WriterState,
    metric: Metric,
    query: &[f32],
    query_norm: f32,
    coarse: &[(crate::core::types::RowId, SlotId)],
    k: usize,
) -> Vec<(crate::core::types::RowId, SlotId)> {
    let mut top = TopK::new(k, metric);
    for &(rowid, slot) in coarse {
        let Some(data) = ws.slots.get(slot.get() as usize) else {
            continue;
        };
        let score = metric.score(query, &data.vector, query_norm, data.norm_sq);
        top.push(score, (rowid, slot));
    }
    top.into_sorted_vec()
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
    collect_access_deltas(ws, &in_segment, now_ms, &mut entries);
    if !full_relations {
        collect_edge_deltas(ws, now_ms, &mut entries);
    }
    entries
}

/// 访问增量条目:最新版本随本段物化时跳过(记录体已携带统计)。
fn collect_access_deltas(
    ws: &WriterState,
    in_segment: &BitSet,
    now_ms: i64,
    entries: &mut Vec<DeltaEntry>,
) {
    for (&rowid, &access_delta) in ws.access_dirty.iter() {
        // 最新版本随本段落盘时,记录体的 access 列已携带最新统计。
        if let Some(latest) = ws.latest.get(&rowid)
            && in_segment.get(latest.get() as usize)
        {
            continue;
        }
        let stat = ws.access.get(&rowid).copied().unwrap_or_default();
        let ns_id = ns_of(ws, rowid);
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
}

/// 关系净变更条目(全量重写关系表时不需要)。
fn collect_edge_deltas(ws: &WriterState, now_ms: i64, entries: &mut Vec<DeltaEntry>) {
    for &(from, to, kind) in ws.edge_dirty.iter() {
        let edge = ws.out_edges.get(&from).and_then(|edges| {
            edges
                .iter()
                .find(|edge| edge.to == to && edge.kind.0 == kind)
        });
        let ns_id = ns_of(ws, from);
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

/// 该 RowId 最新版本所属命名空间;不存在时为 0。
fn ns_of(ws: &WriterState, rowid: crate::core::types::RowId) -> u32 {
    ws.latest
        .get(&rowid)
        .map_or(0, |slot| ws.slots[slot.get() as usize].ns_id.get())
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
    let IndexFields {
        fields,
        key_field_id,
    } = index_fields(ws, max_fields)?;
    let local = observe_included(ws, config, max_fields, included);
    let mut zmap = msec::encode_zmap(&local.zones, &fields, local.block_count);
    zmap.extend_from_slice(&msec::encode_ttl_map(&local.min_expires));
    Ok(SegmentIndexBlobs {
        field_dict: msec::encode_field_dict(&fields),
        zmap,
        bloom: msec::encode_bloom(&local.bloom, key_field_id),
        inverted: msec::encode_inverted(&local.inv)?,
    })
}

/// 字段字典条目与 bloom 字段编号。
struct IndexFields {
    /// 字段定义 `(名称, 类型)`,按名称排序。
    fields: Vec<(Arc<str>, FieldKind)>,
    /// `key` 字段编号(bloom 使用)。
    key_field_id: u16,
}

/// 字段字典条目与 bloom 字段编号(给 `key` 保留唯一槽位)。
fn index_fields(ws: &WriterState, max_fields: usize) -> Result<IndexFields> {
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
    Ok(IndexFields {
        fields,
        key_field_id,
    })
}

/// 段内局部索引观察结果。
struct LocalIndexes {
    zones: ZoneIndex,
    bloom: BloomSet,
    inv: InvertedIndex,
    min_expires: Vec<i64>,
    block_count: usize,
}

/// 在本段局部槽位编号上观察 zone map / bloom / 倒排与 TTL 块最小值。
fn observe_included(
    ws: &WriterState,
    config: &Config,
    max_fields: usize,
    included: &[usize],
) -> LocalIndexes {
    let block_count = included.len().div_ceil(ZONE_BLOCK_ROWS);
    let mut local = LocalIndexes {
        zones: ZoneIndex::new(max_fields),
        bloom: BloomSet::new(BLOOM_INITIAL_CAPACITY, config.tuning.bloom_fpp),
        inv: InvertedIndex::default(),
        min_expires: vec![i64::MAX; block_count],
        block_count,
    };
    for (local_slot, &idx) in included.iter().enumerate() {
        let slot_data = &ws.slots[idx];
        if slot_data.deleted {
            continue;
        }
        local.zones.observe(local_slot, slot_data);
        if let Some(text) = &slot_data.text {
            local.inv.insert_text(
                SlotId::new(local_slot as u32),
                slot_data.ns_id,
                text,
                ws.stopwords_enabled,
            );
        }
        if let Some(key) = &slot_data.key {
            local.bloom.insert(key.as_str());
        }
        // TTL 块级剪枝:块内 min(expires_at),无 TTL 行记 +∞。
        if let Some(expires) = slot_data.expires_at {
            let block = local_slot / ZONE_BLOCK_ROWS;
            local.min_expires[block] = local.min_expires[block].min(expires);
        }
    }
    local
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
/// `quant` 为查询期粗排副本(图仍由 f32 构建);`None` 为纯 f32 段。
///
/// # Errors
/// 图序列化失败(hidx 长度字段超出格式上限)时返回结构化错误。
fn build_index(
    ws: &WriterState,
    config: &Config,
    included: &[usize],
    quant: Option<QuantCopy>,
    parallelism: usize,
) -> Result<BuiltIndex> {
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
    let index = factory.build(crate::memory::index::IndexBuildRequest {
        nodes: &nodes,
        slot_of: &slot_of,
        params: config.hnsw,
        metric: config.metric,
        quant,
        build_precision: config.build_precision,
        build: crate::core::options::HnswBuildParams::from_tuning(&config.tuning, parallelism),
    })?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::core::meta::json;
    use crate::core::metric::Metric;
    use crate::core::options::{
        CompactionPolicy, Compression, Dimension, HnswParams, InsertMode, Limits, RelationIndex,
        SystemClock, Tuning, VectorFormat,
    };
    use crate::core::types::{Key, NsId, RowId, SeqNo};
    use crate::memory::dedup::Dedup;
    use crate::memory::table::SlotData;

    /// 构造仅 `tuning.field_dict_max` 等字段有意义的最小运行配置。
    fn test_config() -> Config {
        Config {
            dimension: Dimension::new(2).expect("dimension"),
            metric: Metric::Cosine,
            insert_mode: InsertMode::default(),
            dedup: Dedup::default(),
            dedup_threshold: 0.9,
            quantization: VectorFormat::default(),
            hnsw: HnswParams::default(),
            build_precision: crate::core::options::BuildPrecision::default(),
            index_factory: None,
            compaction: CompactionPolicy::default(),
            retention: None,
            retain_interval: None,
            access_flush_interval: Duration::from_secs(60),
            compression: Compression::default(),
            observer: None,
            relation_index: RelationIndex::default(),
            parallelism: 1,
            tuning: Tuning::default(),
            limits: Limits::default(),
            clock: Arc::new(SystemClock),
            read_only: false,
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
            vector: crate::memory::lazy::VectorStorage::owned(Arc::from(
                vec![0.0_f32, 1.0].into_boxed_slice(),
            )),
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
