//! L4 检索层:过滤 DSL、查询计划、BM25 与融合(设计 06)。
//!
//! 本层在 L1 冻结的公开 API 之上补齐"混合检索"能力:DSL 字符串解析
//! ([`Expr::from_str`](crate::Expr::from_str))、JSON 往返、查询计划器
//! (zone map/bloom 下推)、BM25 关键词通道与 RRF/加权融合。执行入口
//! [`SearchBuilder::execute`](crate::SearchBuilder::execute) 的流水线亦归本层,
//! 以保证依赖方向恒为 L4 → L1。
//!
//! # 模块
//!
//! * `parse` —— 递归下降 DSL 解析器。
//! * `display` —— 过滤 AST 的 `Display`(可被解析器读回)。
//! * `json` —— `Meta ↔ Expr` 的 JSON 往返。
//! * `iso` —— ISO 8601 与 Unix 毫秒互转(时间戳字面量)。
//! * `bm25` —— BM25 关键词通道(两遍全局统计)。
//! * `fusion` —— RRF / 加权双通道融合。
//! * `zmap` —— zone map / bloom 块级下推求值。
//! * `plan` —— 查询计划器(块位图 + 候选位图)。
//! * `exec` —— `SearchBuilder::execute` 执行管线。

mod bm25;
mod display;
mod exec;
mod fusion;
mod iso;
mod json;
mod parse;
mod plan;
mod zmap;
