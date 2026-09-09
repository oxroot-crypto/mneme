//! Mneme:面向 AI Agent 超长期记忆层的嵌入型向量存储引擎。
//!
//! 本 crate 采用 L0–L6 渐进式分层;当前实现为 **L0 原语层**([`core`]):
//! 无 I/O、无全局状态、无锁的类型与纯函数,是后续各层的地基。
//!
//! # 模块
//!
//! * [`core`] —— 标识类型、错误、距离度量、SIMD 点积、TopK 堆、varint、元数据与配置类型。
//!
//! # 示例
//!
//! ```
//! use mneme::{Metric, simd};
//!
//! let a = [1.0_f32, 2.0, 3.0, 4.0];
//! let b = [1.0_f32, 1.0, 1.0, 1.0];
//! assert_eq!(simd::dot(&a, &b), 10.0);
//! assert_eq!(Metric::Dot.score(&a, &b, 0.0, 0.0), 10.0);
//! ```

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod core;

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
pub use crate::core::types::{Key, NsId, RowId, SegmentId, SeqNo, SlotId};
pub use crate::core::{simd, varint};
