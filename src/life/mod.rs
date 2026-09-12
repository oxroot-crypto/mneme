//! L5 生命周期层:`src/life/` 承载遗忘、compaction 调度与后台维护编排。
//!
//! 本层只做**策略与编排**:选段、幸存版本筛选、调度节拍;段文件读写与
//! MANIFEST 提交仍由 L2 持久层执行(设计 07 §4)。与 `memory` 门面为
//! 组合根例外(`Mneme`/`Builder` 装配时引用本层)。

pub(crate) mod compact;
pub(crate) mod maintenance;
