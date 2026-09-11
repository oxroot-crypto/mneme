# AGENTS.md

Mneme:纯 Rust 的嵌入式向量存储引擎(面向 AI Agent 超长期记忆)。单 crate,edition 2024,
MSRV 1.93。**当前实现了 L0 原语层 `src/core/`、L1 内存引擎 `src/memory/`、L2 持久层
`src/persist/`、L3 索引层 `src/index/`、L4 检索层 `src/query/` 与 L5 生命周期层
`src/life/`**(WAL 轮转、增量段、MANIFEST、崩溃恢复、size-tiered compaction、自研 HNSW、
过滤三档、hidx 图持久化、mmap 段读取、过滤 DSL、zone map/bloom/ttl_map 计划器、BM25、
RRF/加权融合、msec 轻量索引四区与 delta 区、后台维护线程、命名空间/快照/备份/统计);
L6 尚无代码。L2 依赖 `crc32fast`;L3 经 feature `mmap`(默认开)引入 `memmap2`,
`criterion` 为 dev-dependency。

## 契约优先工作流(FSVDD,强制)

任何涉及磁盘格式、API 语义、状态流转、并发或错误的改动,必须按此顺序:

1. 先改 `docs/spec/contracts.md`(FC-Matrix,形式化约束的唯一真实数据源);
2. 再加/改测试,测试头部列出覆盖的 `I1–I30` 不变量与 `FC-*` 编号,函数注释引用编号;
3. 最后改实现。

禁止"先写实现后补契约",禁止契约漂移。新增业务逻辑至少登记一条 `FC-*` 且配 1:1 测试。
改算法/数据结构要同步维护 `contracts.md` §9 的 `FC-*-CPLX-*` 复杂度上界。
未实现测试的条目填 `待补`,不要编造测试文件名占位。

## 分层与代码边界

- L0→L6 只允许向下依赖;新代码必须落在正确的层,不能让下层依赖上层。
  **已文档化例外**:门面/组合根(`memory::Builder`/`Mneme`)为装配需要可引用 `persist`、
  `index` 与 `life`(`Builder` 注入 `Store` 与 `IndexFactory`,`Mneme` 持有维护句柄与
  compaction 门面),不改变 L0→L6 的业务依赖方向。
- `src/core/`(L0)无 I/O、无全局状态、无锁,只有类型与纯函数。
- `unsafe` 只允许两处:`src/core/simd.rs` 的 arch 内联与 `src/persist/source.rs` 的 `MmapSource`
  (mmap 固有 unsafe);每处必须附 `// SAFETY:` 证明。
- L1 起公开 API 冻结;改签名需单独 RFC 并同步 `docs/design/16-api-reference.md`。
- `#![deny(missing_docs)]` + `#![deny(unsafe_op_in_unsafe_fn)]`:新增公开项必须有 rustdoc。

## 依赖与算法

- 依赖白名单:`Cargo.toml` 非 feature 直接依赖白名单上限 4 个(`serde`/`serde_json`/`thiserror`,以及 L2 的 `crc32fast`);L3 的 `memmap2` 随 feature `mmap`(默认开)引入,关后可回退 `FileSource`。新增任何外部依赖都要在 PR 中论证必要性。
- HNSW、BM25、量化、bloom、compaction 调度、分词等复杂算法一律自研。

## 命令

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test                 # 单测 + 集成 + doctest
cargo doc --no-deps        # 公开项须 100% 文档覆盖
```

提交前按 fmt → clippy → test → doc 顺序跑。当前全绿,可作为基线。

## 尚未可用,别踩

- `[features]` 已定义:`mmap`(默认开,引入 `memmap2`);`async`/`quant-f16`/`encrypt`/`compress` 等
  仍待对应层落地,`cargo test --features async` 目前会因 feature 未定义报错。
- `benches/hnsw.rs`(criterion,dev-dependency)已引入;`cargo bench` 的正式门槛/趋势图仍待 CI。
- 契约追溯门禁为 `tests/contract_traceability.rs`(随 `cargo test` 运行,校验契约↔测试双向映射);仓库内**没有** `xtask/` 或 `xtask check-contracts`,不要试图运行。
- 仓库内没有 CI 配置文件(`.gitlab-ci.yml` 等均缺失);CI 四档定义只在 `docs/design/14-testing.md §7`。

## 测试

- 契约测试:`tests/core_contracts.rs`(L0)、`tests/memory_contracts.rs`、
  `tests/query_contracts.rs`、`tests/model_contracts.rs`、`tests/life_contracts.rs`(L1)、
  `tests/persist_contracts.rs`(L2)、`tests/hnsw_contracts.rs`(L3)、`tests/l4_contracts.rs`(L4);
  追溯门禁 `tests/contract_traceability.rs`;属性测试用 `proptest`(dev-dependency)。
- 测试即文档:文件头列不变量编号,断言处引用 `FC-*`,与 `contracts.md` 双向可追溯(门禁强制:无悬空引用、无孤立测试)。

## 文档

- 全部文档为中文。设计文档用 mdBook 组织(`book.toml`,源目录 `docs/`,侧栏 `docs/SUMMARY.md`)。
- 本地预览:`cargo install mdbook mdbook-mermaid` + `cargo install mdbook-katex --no-default-features --features duktape`
  (Windows MSVC 无法编译 katex 默认的 quick-js 后端,必须用 duktape),然后 `mdbook serve`。
- `book/`、`book.tar.gz`、`mermaid.min.js`、`mermaid-init.js` 是构建产物且已 gitignore,不要提交。
- 改设计文档要同步更新 `docs/DESIGN.md` 的目录/交叉引用;公式统一 KaTeX。

## 关键文件

- `docs/DESIGN.md` — 分层地图与阅读路线总入口
- `docs/spec/contracts.md` — FC-Matrix(改动前必读)
- `docs/design/14-testing.md` — 每层验收标准、CI 结构
- `CONTRIBUTING.md` — 基本规则、提交信息格式(Conventional Commits,首行 ≤72 字符,正文说明"为什么")

只在用户明确要求时才提交。复杂任务先规划再动手。
