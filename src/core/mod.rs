//! L0 原语层:类型、错误、距离数学与基础算法。
//!
//! 本层是整库的"地基":**没有任何 I/O、没有全局状态、没有锁**,只有类型与纯函数。
//! 唯一允许出现 `unsafe` 的位置是 [`simd`] 的 arch 内联,且每处均附 `// SAFETY:` 证明。
//!
//! # 模块
//!
//! * [`types`] —— 六类 newtype 标识符(稳定逻辑身份与段内物理槽位分离)。
//! * [`error`] —— 统一错误枚举 [`MnemeError`](error::MnemeError) 与 [`Result`](error::Result)。
//! * [`metric`] —— 三种距离度量,统一归结为一次点积。
//! * [`simd`] —— 手写点积内核 + 运行时分发。
//! * [`heap`] —— `TopK` 有界堆(支持并行归并)。
//! * [`varint`] —— 变长整数编解码。
//! * [`meta`] —— 元数据(JSON)隔离区。
//! * [`options`] —— 全局配置与选项类型。

pub mod error;
pub mod heap;
pub mod meta;
pub mod metric;
pub mod options;
pub mod simd;
pub mod types;
pub mod varint;
