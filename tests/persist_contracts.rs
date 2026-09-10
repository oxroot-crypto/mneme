//! L2 持久层契约验收测试。
//!
//! 覆盖不变量 I1–I4、I15、I16、I18–I20 与对应 `FC-PERSIST-*` 契约(见
//! `docs/spec/contracts.md` §2/§9.2.3)。文件头引用的 `FC-*` 编号必须与契约矩阵中
//! 引用本文件的条目双向相等,由 `tests/contract_traceability.rs` 机械校验。
//!
//! 待 M1 起逐条落地:编解码往返/损坏检出、WAL 组提交与回放、open/flush/close、
//! delta 覆盖持久性、MANIFEST 原子性与崩溃注入。
