//! size-tiered compaction 计划与幸存版本筛选(设计 07 §4)。
//!
//! - **选段**:同层段数 ≥ `tier_count` 时合并该层最小的 `tier_count` 个段;
//!   任一段墓碑+过期占比 > `dead_ratio` 时优先单独重写该段(回收空间)。
//! - **幸存筛选**:全局最新版本(含最新墓碑)始终保留;历史版本在
//!   `history_horizon` 窗口内保留(默认永久);整链为超期墓碑/整链逻辑过期
//!   且窗口外时回收整个 RowId。逐版本 TTL 过滤会令旧版本复活,故过期回收
//!   必须整链判定(设计 07 §4.2a 的正确性口径)。
//!
//! 子模块:选段/死比率见 [`mod@plan`],幸存筛选与整链判定见 [`survivors`]。

mod plan;
mod survivors;

pub(crate) use plan::{SegmentInfo, plan, segment_dead_ratios};
pub(crate) use survivors::{SurvivorSet, select_survivors};

#[cfg(test)]
mod tests;
