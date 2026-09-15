//! 过滤 DSL 解析器(设计 06 §1)。
//!
//! 手写递归下降,优先级 `not > and > or`;接受 `&&` / `||` / `!` 别名,
//! 以及 `always` / `never` 真值常量(供 `Display` 往返闭合)。对任意输入返回
//! 结构化 [`MnemeError::FilterParse`](crate::MnemeError::FilterParse) 且带位置,
//! 绝不 panic(I7)。
//!
//! 值字面量(数字/字符串/时间)的解析见 [`literal`](self::literal)。

mod grammar;
mod literal;
mod parser;

// 旧路径 `crate::query::parse::parse_at` 被 `display` / `json` 的测试引用,保持可达;
// 非测试构建无显式引用,放行警告。
#[allow(unused_imports)]
pub(crate) use parser::parse_at;

#[cfg(test)]
use crate::core::error::MnemeError;
#[cfg(test)]
use crate::memory::pred::{Expr, Val};
#[cfg(test)]
use parser::MAX_DEPTH;

#[cfg(test)]
mod tests;
