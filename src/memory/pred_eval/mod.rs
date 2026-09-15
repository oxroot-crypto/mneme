//! 过滤 AST 的三值求值(`pred_eval/`)。
//!
//! 从 `pred/` 拆出的求值器;采用 Kleene 三值逻辑,顶层仅 `True` 命中
//! (设计 03 §5.1、FC-QUERY-ERR-002)。

mod cmp;
mod eval;
mod glob;
mod resolve;
mod tri;

pub(crate) use eval::matches;
pub(crate) use resolve::is_reserved_field;

#[cfg(test)]
pub(crate) use eval::{contains_evals, reset_contains_evals};
#[cfg(test)]
pub(crate) use glob::glob_match;
#[cfg(test)]
pub(crate) use resolve::{FieldValue, RESERVED_FIELDS, resolve};

#[cfg(test)]
mod tests;
