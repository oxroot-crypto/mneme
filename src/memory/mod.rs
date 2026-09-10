//! L1 内存引擎:全内存的完整公开 API。
//!
//! 本层把 L0 原语组装成一个可用的**纯内存向量库**(易失,适合测试、缓存与
//! 嵌入式临时记忆),公开 API 在此冻结(设计 03 §8)。并发模型为单写者
//! `Mutex<WriterState>` + 读者克隆 `Arc<ReaderView>` 后无锁扫描。
//!
//! # 模块
//!
//! * `engine` —— 库句柄 `Mneme` 的门面方法。
//! * `engine_ops` —— `Mneme` 的统计/fsck/合并控制/落盘方法。
//! * `builder` —— 建库器 `Builder`(链式 setter 在子模块 `builder::options`)。
//! * `namespace` —— `Namespace` 及其写(`write`)/读(`query`)/访问(`access`)/
//!   生命周期(`life`)/关系(`relation`)方法。
//! * `snapshot` —— `SnapshotHandle` 与快照只读视图。
//! * `snapshot_scan` —— 快照视图的遍历/关系读取方法。
//! * `search_builder` —— `SearchBuilder` 链式配置与公开入口 `execute()`。
//! * `search_exec` —— `execute()` 内部执行流水线(视图/扫描/扩展/排序/去重)。
//! * `rerank` —— 融合器 `Fusion` 与精排钩子 `Reranker`。
//! * `expand` —— 关系联想扩展与结果级去重。
//! * `table` —— 内存表、写状态与不可变读视图。
//! * `bitset` —— 标记不可见物理版本的可增长位图。
//! * `search` —— 过滤先行的暴力扫描与并行归并。
//! * `pred` / `pred_eval` —— 过滤 AST 与三值求值。
//! * `dedup` —— 写入期两级去重。
//! * `record` —— 记录与写入/更新结果值类型。
//! * `write_helpers` / `mutate_helpers` —— 写路径与写后变更内部自由函数。
//! * `relation` —— 关系边与联想扩展参数。
//! * `temporal` —— 双时态历史视图。
//! * `score` —— 综合打分、MMR 与记忆沉淀。
//! * `lifecycle` —— 遗忘策略与报告。
//! * `ops` —— 运维报告与运行统计类型。
//! * `config` —— 建库配置。

mod bitset;
mod builder;
// `config`/`dedup`/`relation`/`search`/`table` 供 L2 `persist` 读取内存表结构
// (flush/recover 需要),故以 `pub(crate)` 暴露给同 crate 的兄弟模块。
pub(crate) mod config;
pub(crate) mod dedup;
mod engine;
mod engine_ops;
mod expand;
mod lifecycle;
mod mutate_helpers;
mod namespace;
pub(crate) mod ops;
mod pred;
mod pred_eval;
mod record;
pub(crate) mod relation;
mod rerank;
mod score;
pub(crate) mod search;
mod search_builder;
mod search_exec;
mod snapshot;
mod snapshot_scan;
pub(crate) mod table;
mod temporal;
mod write_helpers;

pub use builder::Builder;
pub use dedup::{Dedup, ResultDedup};
pub use engine::Mneme;
pub use lifecycle::{RetainReport, Retention};
pub use namespace::Namespace;
pub use ops::{
    BackupReport, CheckReport, CompactionControl, CompactionState, Histogram, HistoryStat, NsStat,
    QuantStat, SegmentStat, Stats, StorageStat,
};
pub use pred::{CmpOp, Expr, FieldBuilder, Val};
pub use record::{Hit, InsertOutcome, Record, RecordRef, UpdateOutcome};
pub use relation::{Edge, RelateOptions, RelationExpand};
pub use rerank::{Fusion, QueryCtx, Reranker};
pub use score::{ConsolidateReport, ConsolidationPolicy, ScoreBreakdown, Summarizer};
pub use search_builder::SearchBuilder;
pub use snapshot::{SnapshotHandle, SnapshotNamespace};
pub use table::AccessStat;
