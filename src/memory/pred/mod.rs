//! 过滤 AST 与组合器(`pred/`)。
//!
//! 与 L4 共用同一套 AST;L1 提供 builder 组合器(`Expr::field("x").gt(0.5)`、
//! `&`/`|` 运算符重载),**字符串 DSL 解析器在 L4**([06 §1])。求值器见
//! [`pred_eval`](crate::memory::pred_eval)。

mod ast;
mod builder;
mod plan;

pub(crate) use crate::memory::pred_eval::matches;
#[cfg(test)]
pub(crate) use crate::memory::pred_eval::{
    contains_evals as contains_eval_count, reset_contains_evals,
};
pub(crate) use ast::EvalCtx;
pub use ast::{CmpOp, Expr, Val};
pub use builder::FieldBuilder;
pub(crate) use plan::reorder_for_eval;

#[cfg(test)]
mod tests;
