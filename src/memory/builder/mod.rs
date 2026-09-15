//! 建库器 `Builder`(`builder/`)。
//!
//! 收集建库配置并产出 [`Mneme`](crate::memory::Mneme);字段语义见设计 16 §2。
//! 链式配置 setter 在子模块 [`options`](self::options) 中实现。
//!
//! # 子模块
//!
//! * `build` —— 建库入口 [`Builder::build`] 与后端打开、配置定型、物理表装配。
//! * `model` —— [`Builder`] 类型定义、缺省值与调试输出。
//! * `options` —— 链式配置 setter。
//! * `validate` —— 跨字段配置校验(`FC-GLOBAL-PRE-004`、`FC-INDEX-PRE-001` 等)。

mod build;
mod model;
mod options;
mod validate;

pub use model::Builder;
