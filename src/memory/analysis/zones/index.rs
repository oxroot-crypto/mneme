//! zone map 索引类型与查询侧读取:一次定位取块统计切片,供块级下推使用。
//!
//! 观察(写入)逻辑见 `observe`,统计原语见 `stat`。

use std::collections::HashMap;
use std::sync::Arc;

use crate::memory::analysis::zones::stat::FieldZones;
use crate::memory::analysis::zones::{BlockStat, ZoneKind};

/// 内存 zone map 索引。
#[derive(Clone)]
pub(crate) struct ZoneIndex {
    pub(super) fields: HashMap<Arc<str>, FieldZones>,
    pub(super) max_fields: usize,
}

impl ZoneIndex {
    /// 新建空索引;`max_fields` 为可索引字段上限(超出者只做行级求值)。
    pub(crate) fn new(max_fields: usize) -> Self {
        Self {
            fields: HashMap::new(),
            max_fields,
        }
    }

    /// 取某字段某块的统计;字段未索引或块越界返回 `None`。
    pub(crate) fn block_stat(&self, name: &str, block: usize) -> Option<BlockStat> {
        self.fields
            .get(name)
            .and_then(|field| field.blocks.get(block))
            .copied()
    }

    /// 一次定位取某字段的块统计切片与类别(查询期热路径免逐块哈希)。
    ///
    /// 字段未索引或出现过类型冲突时返回 `None`(调用方据此保持全 1 位图,
    /// 绝不按不完整统计误剪)。
    pub(crate) fn field_zones(&self, name: &str) -> Option<(&[BlockStat], ZoneKind)> {
        self.fields
            .get(name)
            .filter(|field| !field.mixed)
            .map(|field| (field.blocks.as_slice(), field.kind))
    }

    /// 一次定位取某字段的块统计切片(不做 `mixed` 过滤)。
    ///
    /// 供只依赖块统计本身、不依赖字段类别的保守判定使用(如 TTL 块级剪枝):
    /// 与逐块 [`block_stat`](Self::block_stat) 口径一致,含类型冲突字段的统计。
    pub(crate) fn field_blocks(&self, name: &str) -> Option<&[BlockStat]> {
        self.fields.get(name).map(|field| field.blocks.as_slice())
    }

    /// 已索引的全部字段(落盘编码用)。
    pub(crate) fn fields_iter(&self) -> impl Iterator<Item = (&str, ZoneKind)> + '_ {
        self.fields
            .iter()
            .map(|(name, field)| (name.as_ref(), field.kind))
    }
}
