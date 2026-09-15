//! 内存引擎的统计与自检门面(`engine_ops/`)。
//!
//! 以 `impl Mneme` 扩展 [`Mneme`](crate::memory::engine::Mneme) 的运维方法
//! (运行统计、fsck、合并控制、落盘空操作),与库生命周期方法分置于不同文件。

mod check;
mod compact;
mod maintenance;
mod stats;
