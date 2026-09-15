//! 段视图解析与版本链/关系边重建(`recover/segment/`)。
//!
//! # 子模块
//!
//! * `view` —— 段视图解析同 payload 校验。
//! * `version` —— 版本行解码、并行调度同版本链提交。
//! * `slot` —— 活槽位同墓碑槽位构造。
//! * `relation` —— 关系边重建同 delta 回放。

mod relation;
mod slot;
mod version;
mod view;

// 拆分唔改外部可达性:旧路径 `crate::persist::recover::segment::{...}` 照旧。
pub(super) use relation::{apply_delta, apply_relations};
pub(super) use slot::{SlotFromEntry, slot_from_entry};
pub(super) use version::apply_versions;
pub(super) use view::load_segment_views;
