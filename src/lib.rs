//! Mneme:面向 AI Agent 超长期记忆层的嵌入型向量存储引擎。
//!
//! 本 crate 采用 L0–L6 渐进式分层;当前实现为 **L0 原语层**([`core`])、
//! **L1 内存引擎**([`memory`])、**L2 持久层**(`persist`)、**L3 索引层**(`index`)
//! 与 **L4 检索层**(`query`)。L1 提供全内存的完整公开 API(易失);L2 以
//! `Builder::path` 或 [`Mneme::open`] 打开本地目录库,提供 WAL、段文件、
//! MANIFEST 与崩溃恢复;L3 引入自研 HNSW;L4 补齐过滤 DSL、BM25 与 RRF 融合。
//!
//! # 模块
//!
//! * [`core`] —— 标识类型、错误、距离度量、SIMD 点积、TopK 堆、位图、varint、元数据与配置类型。
//! * [`memory`] —— 内存表、暴力检索、过滤 AST、去重与记忆生命周期,并承载公开门面。
//! * `persist`(内部)—— L2 持久层:WAL、段文件、MANIFEST、恢复与全量快照 flush。
//! * `index`(内部)—— L3 索引层:自研 HNSW、过滤三档、hidx 编解码与 mmap 段读取。
//! * `query`(内部)—— L4 检索层:过滤 DSL、查询计划器、BM25、融合与执行管线。
//!
//! # 示例
//!
//! ```
//! use mneme::{Metric, Record, simd};
//!
//! let a = [1.0_f32, 2.0, 3.0, 4.0];
//! let b = [1.0_f32, 1.0, 1.0, 1.0];
//! assert_eq!(simd::dot(&a, &b), 10.0);
//! assert_eq!(Metric::Dot.score(&a, &b, 0.0, 0.0), 10.0);
//!
//! let db = mneme::Mneme::in_memory(2).unwrap();
//! let ns = db.namespace("demo");
//! ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
//! let hits = ns.search().vector(&[1.0, 0.0]).top_k(1).execute().unwrap();
//! assert_eq!(hits.len(), 1);
//! ```

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod core;
pub mod memory;

mod index;
mod persist;
mod query;

pub use crate::core::error::{MnemeError, Result};
pub use crate::core::heap::TopK;
pub use crate::core::meta;
pub use crate::core::meta::{Meta, json};
pub use crate::core::metric::{Metric, Score};
pub use crate::core::options::{
    Clock, CompactionPolicy, Compression, Dimension, Diversity, Feedback, FsyncPolicy, HnswParams,
    InsertMode, Limits, QueryId, RelationIndex, RelationKind, Scoring, SystemClock, TimeAxis,
    Tuning, UpdatePatch, VectorFormat,
};
pub use crate::core::text::tokenize;
pub use crate::core::types::{Key, NsId, RowId, SegmentId, SeqNo, SlotId};
pub use crate::core::{simd, varint};
pub use crate::memory::{
    AccessStat, BackupReport, Builder, CheckReport, CmpOp, CompactionControl, CompactionState,
    ConsolidateReport, ConsolidationPolicy, Dedup, Edge, Expr, FieldBuilder, Fusion, Histogram,
    HistoryStat, Hit, InsertOutcome, Mneme, Namespace, NsStat, QuantStat, QueryCtx, Record,
    RecordRef, RelateOptions, RelationExpand, Reranker, ResultDedup, RetainReport, Retention,
    ScoreBreakdown, SearchBuilder, SegmentStat, SnapshotHandle, SnapshotNamespace, Stats,
    StorageStat, Summarizer, UpdateOutcome, Val,
};
pub use crate::persist::hook::{FsyncHook, IoAction};

/// 对写死在代码里的过滤字面量做运行时解析,失败即 panic(文档化例外,见设计 06 §1)。
///
/// 等价于 `Expr::from_str(...).expect(...)`;处理运行时输入请用
/// [`Expr::from_str`](crate::Expr::from_str)(返回 `Result`,不 panic,I7)。
///
/// # Panics
/// 字面量不是合法过滤 DSL 时 panic(消息含字节位置);仅适用于写死的常量。
///
/// # Examples
/// ```
/// use mneme::{Expr, Mneme, Record, filter};
///
/// let db = Mneme::in_memory(2).unwrap();
/// let ns = db.namespace("demo");
/// ns.insert(
///     Record::new(vec![1.0, 0.0]).metadata(mneme::json!({"kind": "note"})),
/// )
/// .unwrap();
/// let expr: Expr = filter!(r#"kind == "note""#);
/// assert_eq!(ns.count(Some(expr)).unwrap(), 1);
/// ```
#[macro_export]
macro_rules! filter {
    ($expr:expr) => {
        $crate::Expr::from_str($expr).expect("filter!: 过滤表达式字面量非法")
    };
}
