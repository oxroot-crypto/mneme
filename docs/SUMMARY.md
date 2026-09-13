# Summary

- [Mneme 设计文档](DESIGN.md)

---

- [00 零基础篇](design/00-fundamentals.md)
- [01 总览:定位、架构与分层](design/01-overview.md)

---

- [02 L0 原语层](design/02-l0-core.md)
- [03 L1 内存引擎](design/03-l1-memory.md)
- [04 L2 持久层](design/04-l2-persist.md)
- [05 L3 索引层:HNSW 的完整数学](design/05-l3-hnsw.md)
- [06 L4 检索层:DSL、BM25 与混合融合](design/06-l4-query.md)
- [07 L5 生命周期层](design/07-l5-life.md)
- [08 L6 打磨层:量化、两阶段检索与 async 门面](design/08-l6-quant.md)

---

- [09 记忆模型层:关系、双时态、来源与沉淀](design/09-memory-model.md)
- [10 检索排序层:让检索像记忆一样工作](design/10-scoring.md)
- [11 存储安全与压缩:静态加密、文本压缩](design/11-security-storage.md)
- [12 部署形态:多进程只读、WASM 与可观测性](design/12-deployment.md)
- [13 记忆模式手册(Agent 开发者 Cookbook)](design/13-cookbook.md)

---

- [14 测试与验收](design/14-testing.md)
- [15 术语表、符号表与复杂度速查](design/15-glossary.md)
- [16 公开 API 与运维参考](design/16-api-reference.md)

---

- [形式化契约矩阵(FC-Matrix)](spec/contracts.md)

---

- [Rust 零基础教学:导读](rust/README.md)
- [01 工具链与 Cargo](rust/01-toolchain.md)
- [02 值、类型与所有权](rust/02-values-and-ownership.md)
- [03 结构体、枚举与 impl](rust/03-structs-enums-impl.md)
- [04 引用、借用、生命周期与字符串](rust/04-borrowing-strings-slices.md)
- [05 错误处理](rust/05-errors.md)
- [06 泛型与 trait](rust/06-generics-traits.md)
- [07 迭代器与闭包](rust/07-iterators-closures.md)
- [08 模块、可见性与文档](rust/08-modules-docs.md)
- [09 条件编译、unsafe 与 SIMD](rust/09-cfg-unsafe-simd.md)
- [10 测试与属性测试](rust/10-testing.md)
- [11 异步与 tokio 最小封装](rust/11-async-tokio.md)
