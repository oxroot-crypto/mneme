//! 段内 zone map/bloom/倒排同 HNSW 图个构建。

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::types::SlotId;
use crate::memory::analysis::{
    BLOOM_INITIAL_CAPACITY, BloomSet, InvertedIndex, ZONE_BLOCK_ROWS, ZoneIndex,
};
use crate::memory::config::Config;
use crate::memory::index::{IndexNode, QuantCopy, VectorIndex};
use crate::memory::table::WriterState;
use crate::persist::msec::{self, FieldKind};

/// 由本次物化的槽位构建局部 zone map / bloom / 倒排在段内局部编号上编码。
///
/// 全局加速结构(`ws.zones`/`ws.inv`/`ws.key_bloom`)覆盖全部槽位,块编号与
/// 段内块不一致,不可直接编码;增量段按局部槽位重建这三区(代价正比于本段行数)。
///
/// # Errors
/// 倒排编码超 `u32` 长度或 postings 违背升序不变量时返回结构化错误。
pub(super) fn build_indexes(
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
pub(super) struct SegmentIndexBlobs {
    pub(super) field_dict: Vec<u8>,
    pub(super) zmap: Vec<u8>,
    pub(super) bloom: Vec<u8>,
    pub(super) inverted: Vec<u8>,
}

/// 一次索引构建的产物(hidx 字节、内存索引与入口)。
pub(super) struct BuiltIndex {
    pub(super) bytes: Option<Vec<u8>>,
    pub(super) index: Option<Arc<dyn VectorIndex>>,
    pub(super) entry_slot: u32,
    pub(super) entry_level: u8,
}

/// [`build_index`] 的输入参数。
pub(super) struct BuildIndexInput<'a> {
    /// 写状态(节点向量与行标识来源)。
    pub(super) ws: &'a WriterState,
    /// 库配置(HNSW 参数与索引工厂)。
    pub(super) config: &'a Config,
    /// 本次物化的全局槽位(升序;节点 id = 下标)。
    pub(super) included: &'a [usize],
    /// 段量化副本(`None` = 纯 f32),只服务查询期粗排打分。
    pub(super) quant: Option<QuantCopy>,
    /// 建图并行度。
    pub(super) parallelism: usize,
}

/// 构建 HNSW 图并序列化为 hidx(无工厂或空段时各字段为空/零)。
///
/// `quant` 为查询期粗排副本(图仍由 f32 构建);`None` 为纯 f32 段。
///
/// # Errors
/// 图序列化失败(hidx 长度字段超出格式上限)时返回结构化错误。
pub(super) fn build_index(input: BuildIndexInput<'_>) -> Result<BuiltIndex> {
    let BuildIndexInput {
        ws,
        config,
        included,
        quant,
        parallelism,
    } = input;
    let Some(factory) = config.index_factory.as_ref() else {
        return Ok(empty_built_index());
    };
    if included.is_empty() {
        return Ok(empty_built_index());
    }
    let (nodes, slot_of) = collect_index_nodes(ws, included);
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

/// 空构建产物(无工厂或空段):各字段为空/零。
fn empty_built_index() -> BuiltIndex {
    BuiltIndex {
        bytes: None,
        index: None,
        entry_slot: 0,
        entry_level: 0,
    }
}

/// 按 `included` 顺序收集 HNSW 节点与全局槽位编号。
fn collect_index_nodes(ws: &WriterState, included: &[usize]) -> (Vec<IndexNode>, Vec<SlotId>) {
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
    (nodes, slot_of)
}
