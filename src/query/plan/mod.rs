//! 查询计划器(设计 06 §2)。
//!
//! 把过滤 AST 编译成"每查询一份"的执行方案:块位图(zone map/bloom 下推)
//! 只减少行级求值;候选位图由向量与 BM25 两通道共享(过滤先行)。残余谓词
//! 仍以三值求值为准,故候选集合与逐行求值全等(下推只减少工作量)。

mod compile;
mod filter;

// 旧路径 `crate::query::plan::Plan` 要保持可达;crate 内当前无显式引用,放行警告。
#[allow(unused_imports)]
pub(crate) use compile::{Plan, compile};

#[cfg(test)]
mod tests;
