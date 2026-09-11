//! 块级 zone map 统计(`analysis/zones.rs`,设计 04 §5.2、06 §2)。
//!
//! 每 1024 个物理槽位一块,对数值/时间字段记录块内 min/max 与值/null 存在性。
//! zone map 只用于查询期块级**下推**(剪除必然无匹配的块),最终命中仍以逐行
//! 三值求值为准——统计退化只降低剪枝率,绝不改变过滤语义。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::meta::Meta;
use crate::memory::pred_eval::is_reserved_field;
use crate::memory::table::SlotData;

/// zone map 的块粒度(物理槽位数;与存储块粒度一致)。
pub(crate) const ZONE_BLOCK_ROWS: usize = 1024;

/// 可由 `f64` 精确表示的整数绝对值上界(2^53);超出者按"区间未知"处理,
/// 绝不因精度损失把本可命中的块剪掉(下推只允许漏放,不允许漏报)。
pub(crate) const MAX_EXACT_INT: i64 = 1 << 53;

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
    /// 块内是否存在该字段的**任意**值(含字符串/布尔/对象/数组/null),
    /// 供 `exists` 剪枝;缺失该标志会把有值的块误剪。
    pub(crate) has_any: bool,
}

/// 单字段的全部块统计。
#[derive(Clone)]
struct FieldZones {
    kind: ZoneKind,
    /// 出现类型冲突(同一字段被两种 `ZoneKind` 观察,实现防御):该字段退化为
    /// 不参与块级剪枝,查询侧 `kind_of` 返回 `None`,绝不按不完整的统计误剪。
    mixed: bool,
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
        self.observe_int("created_at", ZoneKind::Ts, slot.created_at, block);
        if let Some(expires_at) = slot.expires_at {
            self.observe_int("expires_at", ZoneKind::Ts, expires_at, block);
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
        self.observe_int("valid_from", ZoneKind::Ts, slot.valid_from, block);
        if let Some(valid_to) = slot.valid_to {
            self.observe_int("valid_to", ZoneKind::Ts, valid_to, block);
        }
        self.observe_meta(&slot.meta, "", block);
    }

    /// 递归收集 metadata 中字段路径的存在性与数值区间(数组不参与路径)。
    ///
    /// 与保留字段同名的 metadata 键**一律跳过**:行级求值以保留值为准,同名
    /// metadata 永远读不到,若把它的统计当剪枝依据会静默漏报(FC-QUERY-POST-005)。
    fn observe_meta(&mut self, value: &Meta, prefix: &str, block: usize) {
        if is_reserved_field(prefix) {
            return;
        }
        match value {
            Meta::Object(map) => {
                if !prefix.is_empty() {
                    self.observe_any(prefix, block);
                }
                for (key, child) in map {
                    if prefix.is_empty() {
                        self.observe_meta(child, key, block);
                    } else {
                        self.observe_meta(child, &format!("{prefix}.{key}"), block);
                    }
                }
            }
            Meta::Number(number) if !prefix.is_empty() => {
                if let Some(int) = number.as_i64() {
                    self.observe_int(prefix, ZoneKind::Num, int, block);
                } else if let Some(value) = number.as_f64() {
                    self.observe_value(prefix, ZoneKind::Num, value, block);
                }
            }
            Meta::Null if !prefix.is_empty() => {
                self.observe_null(prefix, block);
            }
            // 字符串/布尔/数组等非数值类型:只记录"存在",不参与区间剪枝。
            _ if !prefix.is_empty() => self.observe_any(prefix, block),
            _ => {}
        }
    }

    /// 更新单个数值字段的块统计。
    fn observe_value(&mut self, name: &str, kind: ZoneKind, value: f64, block: usize) {
        let Some(field) = self.field_mut(name, kind) else {
            return;
        };
        let stat = block_mut(&mut field.blocks, block);
        stat.has_any = true;
        if stat.has_value {
            stat.min = stat.min.min(value);
            stat.max = stat.max.max(value);
        } else {
            stat.min = value;
            stat.max = value;
            stat.has_value = true;
        }
    }

    /// 更新 `i64` 字段的块统计;超出 `f64` 精确范围时退化为"区间未知"(±∞)。
    fn observe_int(&mut self, name: &str, kind: ZoneKind, value: i64, block: usize) {
        if value.unsigned_abs() > MAX_EXACT_INT as u64 {
            self.observe_lossy(name, kind, block);
        } else {
            self.observe_value(name, kind, value as f64, block);
        }
    }

    /// 把块的区间放宽为 `[-∞, +∞]`:有值但无法精确剪枝时使用,只降剪枝率。
    fn observe_lossy(&mut self, name: &str, kind: ZoneKind, block: usize) {
        let Some(field) = self.field_mut(name, kind) else {
            return;
        };
        let stat = block_mut(&mut field.blocks, block);
        stat.has_any = true;
        stat.has_value = true;
        stat.min = f64::NEG_INFINITY;
        stat.max = f64::INFINITY;
    }

    /// 标记字段在某块存在任意值(不参与区间统计)。
    fn observe_any(&mut self, name: &str, block: usize) {
        let Some(field) = self.field_mut(name, ZoneKind::Num) else {
            return;
        };
        block_mut(&mut field.blocks, block).has_any = true;
    }

    /// 标记字段在某块出现显式 null。
    fn observe_null(&mut self, name: &str, block: usize) {
        let Some(field) = self.field_mut(name, ZoneKind::Num) else {
            return;
        };
        let stat = block_mut(&mut field.blocks, block);
        stat.has_any = true;
        stat.has_null = true;
    }

    /// 取(或按需注册)字段;类型冲突时标记 `mixed` 并返回 `None`,超上限返回 `None`。
    fn field_mut(&mut self, name: &str, kind: ZoneKind) -> Option<&mut FieldZones> {
        if !self.fields.contains_key(name) {
            if self.fields.len() >= self.max_fields {
                return None;
            }
            self.fields.insert(
                Arc::from(name),
                FieldZones {
                    kind,
                    mixed: false,
                    blocks: Vec::new(),
                },
            );
        }
        let field = self.fields.get_mut(name)?;
        if field.kind == kind {
            return Some(field);
        }
        // 类型冲突:保留已有统计但放弃块级剪枝(统计已不完整,继续用会漏报)。
        field.mixed = true;
        None
    }

    /// 取某字段某块的统计;字段未索引或块越界返回 `None`。
    pub(crate) fn block_stat(&self, name: &str, block: usize) -> Option<BlockStat> {
        self.fields
            .get(name)
            .and_then(|field| field.blocks.get(block))
            .copied()
    }

    /// 字段的取值类别;未索引或出现过类型冲突返回 `None`(不参与块级剪枝)。
    pub(crate) fn kind_of(&self, name: &str) -> Option<ZoneKind> {
        self.fields
            .get(name)
            .filter(|field| !field.mixed)
            .map(|field| field.kind)
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
        assert!(note.has_any, "null 也是存在的取值");
        assert!(zones.kind_of("note").is_some());
    }

    #[test]
    fn marks_existence_for_non_numeric_values() {
        let mut zones = ZoneIndex::new(16);
        zones.observe(0, &slot(0, 0.5, json!({"note": "s", "flag": true})));
        let note = zones.block_stat("note", 0).expect("note");
        assert!(note.has_any, "字符串字段必须记录存在性");
        assert!(!note.has_value, "字符串不参与数值区间");
        assert!(!note.has_null);
        let flag = zones.block_stat("flag", 0).expect("flag");
        assert!(flag.has_any, "布尔字段必须记录存在性");
    }

    /// 大整数(>2^53)无法用 `f64` 精确表示:区间的放宽为 ±∞,绝不误剪。
    #[test]
    fn large_integers_widen_block_interval() {
        let mut zones = ZoneIndex::new(16);
        zones.observe(0, &slot(0, 0.5, json!({"rank": 9_007_199_254_740_993_i64})));
        let rank = zones.block_stat("rank", 0).expect("rank");
        assert!(rank.has_value);
        assert_eq!((rank.min, rank.max), (f64::NEG_INFINITY, f64::INFINITY));
    }

    /// FC-QUERY-POST-005(字段类别冲突必须放弃剪枝,不得静默丢弃统计)
    #[test]
    fn kind_conflict_disables_block_pruning() {
        let mut zones = ZoneIndex::new(16);
        zones.observe_int("conflict", ZoneKind::Ts, 1_000, 0);
        assert_eq!(zones.kind_of("conflict"), Some(ZoneKind::Ts));
        zones.observe_int("conflict", ZoneKind::Num, 7, 0);
        assert_eq!(
            zones.kind_of("conflict"),
            None,
            "类型冲突字段必须退出块级剪枝"
        );
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

    /// FC-QUERY-POST-005(保留字段同名的 metadata 不进 zone map,行级以保留值为准)
    #[test]
    fn reserved_metadata_is_shadowed_and_not_indexed() {
        let mut zones = ZoneIndex::new(16);
        zones.observe(
            0,
            &slot(0, 0.5, json!({"created_at": 5, "key": 9, "rowid": 99})),
        );
        assert_eq!(
            zones.kind_of("created_at"),
            Some(ZoneKind::Ts),
            "created_at 的统计只来自保留值"
        );
        assert!(zones.kind_of("key").is_none(), "保留名 metadata 不注册");
        assert!(zones.kind_of("rowid").is_none(), "保留名 metadata 不注册");
        let stat = zones.block_stat("created_at", 0).expect("created_at");
        assert!(stat.min >= 1_000.0, "不得混入 metadata 的 5");
    }
}
