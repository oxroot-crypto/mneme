//! 建库器 [`Builder`](super::Builder) 的链式配置 setter(`builder/options/`)。
//!
//! 仅承载参数写入,不包含校验逻辑(校验收敛在 [`Builder::build`](super::Builder::build));
//! 字段语义见设计 16 §2。
//!
//! # 子模块
//!
//! * `storage` —— 库身份、存储后端与打开可靠性 setter。
//! * `write` —— 写入语义与索引相关 setter。
//! * `lifecycle` —— 后台维护、遗忘、观测与调参 setter。
//!
//! 拆分不改外部可达性:原文件条目均为 [`Builder`](super::Builder) 的固有方法
//! (无模块级公开条目),旧路径 `crate::memory::builder::options` 可达性照旧。

mod lifecycle;
mod storage;
mod write;
