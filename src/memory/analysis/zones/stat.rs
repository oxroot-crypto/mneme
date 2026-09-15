//! zone map 的块级统计原语:字段类别、块统计与字段容器。
//!
//! 只定义数据形态与块下标扩容纯函数;观察(写入)与查询读取分见 `observe` /
//! `index`。

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
pub(super) struct FieldZones {
    pub(super) kind: ZoneKind,
    /// 出现类型冲突(同一字段被两种 `ZoneKind` 观察,实现防御):该字段退化为
    /// 不参与块级剪枝,查询侧 `kind_of` 返回 `None`,绝不按不完整的统计误剪。
    pub(super) mixed: bool,
    pub(super) blocks: Vec<BlockStat>,
}

/// 取块统计,必要时扩容;扩容后下标必然有效。
pub(super) fn block_mut(blocks: &mut Vec<BlockStat>, block: usize) -> &mut BlockStat {
    if blocks.len() <= block {
        blocks.resize(block + 1, BlockStat::default());
    }
    &mut blocks[block]
}
