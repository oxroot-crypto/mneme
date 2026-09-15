//! zone map 的观察(写入)侧:逐槽位更新块级 min/max 与值/null/存在性统计。
//!
//! 索引类型与查询读取见 `index`,统计原语见 `stat`。

use std::sync::Arc;

use crate::core::meta::Meta;
use crate::memory::analysis::zones::stat::{FieldZones, block_mut};
use crate::memory::analysis::zones::{MAX_EXACT_INT, ZONE_BLOCK_ROWS, ZoneIndex, ZoneKind};
use crate::memory::pred_eval::is_reserved_field;
use crate::memory::table::SlotData;

impl ZoneIndex {
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
    /// 保留名**自身**不注册(行级读 `SlotData` 保留值,同名 metadata 永远读不到),
    /// 但其对象子路径(如 `key.x`)按 `meta::get_path` 语义照常观察(FC-QUERY-POST-005)。
    fn observe_meta(&mut self, value: &Meta, prefix: &str, block: usize) {
        let reserved = is_reserved_field(prefix);
        match value {
            Meta::Object(map) => {
                if !prefix.is_empty() && !reserved {
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
            Meta::Number(number) if !prefix.is_empty() && !reserved => {
                if let Some(int) = number.as_i64() {
                    self.observe_int(prefix, ZoneKind::Num, int, block);
                } else if let Some(value) = number.as_f64() {
                    self.observe_value(prefix, ZoneKind::Num, value, block);
                }
            }
            Meta::Null if !prefix.is_empty() && !reserved => {
                self.observe_null(prefix, block);
            }
            // 字符串/布尔/数组等非数值类型:只记录"存在",不参与区间剪枝。
            _ if !prefix.is_empty() && !reserved => self.observe_any(prefix, block),
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
    pub(super) fn observe_int(&mut self, name: &str, kind: ZoneKind, value: i64, block: usize) {
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
}
