//! 块级 zone map 统计(`analysis/zones.rs`,设计 04 §5.2、06 §2)。
//!
//! 每 1024 个物理槽位一块,对数值/时间字段记录块内 min/max 与值/null 存在性。
//! zone map 只用于查询期块级**下推**(剪除必然无匹配的块),最终命中仍以逐行
//! 三值求值为准——统计退化只降低剪枝率,绝不改变过滤语义。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::meta::Meta;
use crate::memory::table::SlotData;

/// zone map 的块粒度(物理槽位数;与存储块粒度一致)。
pub(crate) const ZONE_BLOCK_ROWS: usize = 1024;

/// 字段取值类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZoneKind {
    /// 普通数值。
    Num,
    /// Unix 毫秒时间戳(与 `Val::Ts` 对应)。
    Ts,
}

/// 单块单字段的统计。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct BlockStat {
    /// 块内最小值(仅当 `has_value`)。
    pub(crate) min: f64,
    /// 块内最大值(仅当 `has_value`)。
    pub(crate) max: f64,
    /// 块内是否存在该字段的数值。
    pub(crate) has_value: bool,
    /// 块内是否存在显式 JSON null(供 `is_null` 剪枝)。
    pub(crate) has_null: bool,
}

/// 单字段的全部块统计。
#[derive(Clone)]
struct FieldZones {
    kind: ZoneKind,
    blocks: Vec<BlockStat>,
}

/// 内存 zone map 索引。
#[derive(Clone)]
pub(crate) struct ZoneIndex {
    fields: HashMap<Arc<str>, FieldZones>,
    max_fields: usize,
}

impl ZoneIndex {
    /// 新建空索引;`max_fields` 为可索引字段上限(超出者只做行级求值)。
    pub(crate) fn new(max_fields: usize) -> Self {
        Self {
            fields: HashMap::new(),
            max_fields,
        }
    }

    /// 观察一条物理槽位,更新块级统计。
    ///
    /// # Arguments
    /// * `slot_index` - 全局物理槽位下标(块号 = `slot_index / 1024`)。
    /// * `slot` - 槽位数据(取保留字段与 metadata 数值)。
    pub(crate) fn observe(&mut self, slot_index: usize, slot: &SlotData) {
        let block = slot_index / ZONE_BLOCK_ROWS;
        self.observe_value("created_at", ZoneKind::Ts, slot.created_at as f64, block);
        if let Some(expires_at) = slot.expires_at {
            self.observe_value("expires_at", ZoneKind::Ts, expires_at as f64, block);
        }
        self.observe_value(
            "importance",
            ZoneKind::Num,
            f64::from(slot.importance),
            block,
        );
        self.observe_value(
            "confidence",
            ZoneKind::Num,
            f64::from(slot.confidence),
            block,
        );
        self.observe_value("valid_from", ZoneKind::Ts, slot.valid_from as f64, block);
        if let Some(valid_to) = slot.valid_to {
            self.observe_value("valid_to", ZoneKind::Ts, valid_to as f64, block);
        }
        self.observe_meta(&slot.meta, "", block);
    }

    /// 递归收集 metadata 中的数值叶子路径(数组不参与路径)。
    ///
    /// 与保留字段同名的 metadata 键会走同一 zone:同名值只扩大区间上界,
    /// 行级求值仍以保留字段为准,故不影响正确性(仅降低剪枝率)。
    fn observe_meta(&mut self, value: &Meta, prefix: &str, block: usize) {
        match value {
            Meta::Object(map) => {
                for (key, child) in map {
                    if prefix.is_empty() {
                        self.observe_meta(child, key, block);
                    } else {
                        self.observe_meta(child, &format!("{prefix}.{key}"), block);
                    }
                }
            }
            Meta::Number(number) => {
                if !prefix.is_empty()
                    && let Some(value) = number.as_f64()
                {
                    self.observe_value(prefix, ZoneKind::Num, value, block);
                }
            }
            Meta::Null if !prefix.is_empty() => {
                self.observe_null(prefix, block);
            }
            _ => {}
        }
    }

    /// 更新单个数值字段的块统计。
    fn observe_value(&mut self, name: &str, kind: ZoneKind, value: f64, block: usize) {
        let Some(field) = self.field_mut(name, kind) else {
            return;
        };
        let stat = block_mut(&mut field.blocks, block);
        if stat.has_value {
            stat.min = stat.min.min(value);
            stat.max = stat.max.max(value);
        } else {
            stat.min = value;
            stat.max = value;
            stat.has_value = true;
        }
    }

    /// 标记字段在某块出现显式 null。
    fn observe_null(&mut self, name: &str, block: usize) {
        let Some(field) = self.field_mut(name, ZoneKind::Num) else {
            return;
        };
        block_mut(&mut field.blocks, block).has_null = true;
    }

    /// 取(或按需注册)字段;类型不符或超上限返回 `None`。
    fn field_mut(&mut self, name: &str, kind: ZoneKind) -> Option<&mut FieldZones> {
        if !self.fields.contains_key(name) {
            if self.fields.len() >= self.max_fields {
                return None;
            }
            self.fields.insert(
                Arc::from(name),
                FieldZones {
                    kind,
                    blocks: Vec::new(),
                },
            );
        }
        let field = self.fields.get_mut(name)?;
        (field.kind == kind).then_some(field)
    }

    /// 取某字段某块的统计;字段未索引或块越界返回 `None`。
    pub(crate) fn block_stat(&self, name: &str, block: usize) -> Option<BlockStat> {
        self.fields
            .get(name)
            .and_then(|field| field.blocks.get(block))
            .copied()
    }

    /// 字段的取值类别;未索引返回 `None`。
    pub(crate) fn kind_of(&self, name: &str) -> Option<ZoneKind> {
        self.fields.get(name).map(|field| field.kind)
    }

    /// 已索引的全部字段(落盘编码用)。
    pub(crate) fn fields_iter(&self) -> impl Iterator<Item = (&str, ZoneKind)> + '_ {
        self.fields
            .iter()
            .map(|(name, field)| (name.as_ref(), field.kind))
    }
}

/// 取块统计,必要时扩容;扩容后下标必然有效。
fn block_mut(blocks: &mut Vec<BlockStat>, block: usize) -> &mut BlockStat {
    if blocks.len() <= block {
        blocks.resize(block + 1, BlockStat::default());
    }
    &mut blocks[block]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::meta::json;
    use crate::core::types::{Key, NsId, RowId, SeqNo};
    use std::sync::Arc;

    fn slot(index: usize, importance: f32, meta: Meta) -> SlotData {
        SlotData {
            rowid: RowId::new(index as u64),
            ns_id: NsId::new(1),
            ns_path: Arc::from("n"),
            seqno: SeqNo::new(index as u64 + 1),
            key: Some(Key::new(format!("k{index}"))),
            vector: Arc::from(vec![0.0_f32].into_boxed_slice()),
            norm_sq: 0.0,
            text: None,
            text_hash: None,
            meta,
            created_at: 1_000 + index as i64,
            expires_at: None,
            importance,
            confidence: 1.0,
            valid_from: 1_000,
            valid_to: None,
            provenance: None,
            tx_ms: 1_000,
            deleted: false,
        }
    }

    #[test]
    fn aggregates_min_max_within_block() {
        let mut zones = ZoneIndex::new(16);
        zones.observe(0, &slot(0, 0.2, json!({"rank": 7})));
        zones.observe(1, &slot(1, 0.9, json!({"rank": 3})));
        let stat = zones.block_stat("importance", 0).expect("importance");
        assert_eq!(
            (stat.min, stat.max),
            (f64::from(0.2_f32), f64::from(0.9_f32))
        );
        let rank = zones.block_stat("rank", 0).expect("rank");
        assert_eq!((rank.min, rank.max), (3.0, 7.0));
    }

    #[test]
    fn separates_blocks() {
        let mut zones = ZoneIndex::new(16);
        zones.observe(0, &slot(0, 0.1, json!({})));
        zones.observe(ZONE_BLOCK_ROWS, &slot(1, 0.7, json!({})));
        assert_eq!(
            zones.block_stat("importance", 0).map(|s| s.min),
            Some(f64::from(0.1_f32))
        );
        assert_eq!(
            zones.block_stat("importance", 1).map(|s| s.min),
            Some(f64::from(0.7_f32))
        );
        assert!(zones.block_stat("importance", 2).is_none());
    }

    #[test]
    fn marks_null_and_absent_values() {
        let mut zones = ZoneIndex::new(16);
        zones.observe(0, &slot(0, 0.5, json!({"note": null})));
        let note = zones.block_stat("note", 0).expect("note");
        assert!(note.has_null);
        assert!(!note.has_value);
        assert!(zones.kind_of("note").is_some());
    }

    #[test]
    fn respects_field_limit() {
        // 保留字段占 4 个(created_at/importance/confidence/valid_from),
        // 上限 5 时恰好还能注册一个 metadata 字段(a 与 b 按字典序,a 先)。
        let mut zones = ZoneIndex::new(5);
        zones.observe(0, &slot(0, 0.5, json!({"a": 1, "b": 2})));
        assert!(zones.kind_of("a").is_some());
        assert!(zones.kind_of("b").is_none(), "超上限字段不再注册");
    }

    #[test]
    fn reserved_timestamp_kind_wins_over_metadata_number() {
        let mut zones = ZoneIndex::new(16);
        zones.observe(0, &slot(0, 0.5, json!({"created_at": 5})));
        assert_eq!(zones.kind_of("created_at"), Some(ZoneKind::Ts));
        let stat = zones.block_stat("created_at", 0).expect("created_at");
        assert!(stat.max >= 1_000.0);
    }
}
