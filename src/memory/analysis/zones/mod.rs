//! 块级 zone map 统计(`analysis/zones/`,设计 04 §5.2、06 §2)。
//!
//! 每 1024 个物理槽位一块,对数值/时间字段记录块内 min/max 与值/null 存在性。
//! zone map 只用于查询期块级**下推**(剪除必然无匹配的块),最终命中仍以逐行
//! 三值求值为准——统计退化只降低剪枝率,绝不改变过滤语义。

mod index;
mod observe;
mod stat;

pub(crate) use index::ZoneIndex;
pub(crate) use stat::{BlockStat, MAX_EXACT_INT, ZONE_BLOCK_ROWS, ZoneKind};

#[cfg(test)]
mod tests;
