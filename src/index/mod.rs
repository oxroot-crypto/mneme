//! L3 索引层:自研 HNSW、过滤三档搜索与 hidx 持久化(设计 05)。
//!
//! 本层把 L1 的暴力扫描升级为近似最近邻(HNSW),公开 API 不变;依赖 L0
//! (`bitset`/`TopK`/`Metric`/标识类型)与 L1 的索引抽象
//! [`crate::memory::index`](VectorIndex/IndexFactory),不依赖 L2 字节布局之外的实现。
//!
//! # 模块
//!
//! * [`hnsw`] —— HNSW 构建 / 查询 / 启发式选邻。
//! * [`graph`] —— 分层邻接图存储。
//! * [`filtered`] —— 过滤三档策略(后过滤 / 放大后过滤 / 候选暴力)。
//! * [`rebuild`] —— compaction 整体重建入口。
//! * [`hidx`] —— HID1 文件编解码。
//! * [`factory`] —— HNSW 工厂(组合根注入 `IndexFactory`)。
//!
//! > 跨来源归并(索引前缀 + 未建树尾)复用 L0 的 [`TopK::merge`](crate::core::heap::TopK::merge),
//! > 不单设 `merge.rs`——L2 全量快照下"多段"退化为单图 + 尾扫描,L5 compaction 再引入
//! > 多图归并时恢复该模块(见设计 05 §9)。

pub(crate) mod filtered;
pub(crate) mod graph;
pub(crate) mod hidx;
pub(crate) mod hnsw;
pub(crate) mod rebuild;

mod factory;

pub(crate) use factory::default_factory;
