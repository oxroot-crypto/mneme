//! 物理槽位与写状态(`table/state/`)。
//!
//! # 子模块
//!
//! * `slot` —— 物理槽位 [`SlotData`] 与访问统计 [`AccessStat`]。
//! * `writer` —— 写状态 [`WriterState`]、ID 分配与 flush 记账。
//! * `relation` —— 关系类型注册表与关系边维护。
//! * `version` —— 版本链提交、遮蔽、墓碑与回收剪除。
//! * `index` —— 检索加速结构维护与段索引安装。

mod index;
mod relation;
mod slot;
mod version;
mod writer;

pub(crate) use index::{InstallSegmentInput, build_segment_index};
pub use slot::AccessStat;
pub(crate) use slot::{SlotData, slot_id_for};
pub(crate) use writer::WriterState;

#[cfg(test)]
mod tests;
