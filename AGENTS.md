# AGENTS.md

Mneme:纯 Rust 的嵌入式向量存储引擎(面向 AI Agent 超长期记忆)。单 crate,edition 2024,
MSRV 1.93。**当前实现了 L0 原语层 `src/core/`、L1 内存引擎 `src/memory/`、L2 持久层
`src/persist/`、L3 索引层 `src/index/`、L4 检索层 `src/query/`、L5 生命周期层
`src/life/` 与 L6 打磨层 `src/quant/` + `feature = "async"` 门面**(WAL 轮转、增量段、
MANIFEST、崩溃恢复、size-tiered compaction、自研 HNSW(默认混合精度建图
`BuildPrecision::Hybrid`:i8 临时码流遍历 + f32 精排选邻,可回退全 f32)、过滤三档、hidx 图持久化、
mmap 段读取、过滤 DSL、zone map/bloom/ttl_map 计划器、BM25、RRF/加权融合、msec
轻量索引四区与 delta 区、后台维护线程、命名空间/快照/备份/统计、i8/f16 量化副本 +
两阶段精排 + 建段抽样回退、`AsyncNamespace`)。L2 依赖 `crc32fast`;L3 经 feature
`mmap`(默认开)引入 `memmap2`;L6 经 feature `async` 引入 `tokio`(仅 `rt`)、经
feature `quant-f16` 引入 `half`;L11 静态加密经 feature `encrypt` 引入 `aes-gcm` +
`getrandom`,压缩经 feature `compress`(自研 LZ4 风格,零依赖)/`compress-zstd`
引入 `zstd`;`criterion` 为 dev-dependency。
**L1–L6 收尾与 09/10/11/12 扩充已落地**(段句柄惰性驻留、候选放大/偏置路由、
自定义关系注册表、加密/压缩、`Storage` 后端与只读共享、`Observer`、写视图索引分片/分块
COW);1M×1536
性能门槛与 fuzz 长跑由本机/专用 runner 手动执行(不上 CI),1M 正式门槛待
≥16GB 专用 runner;
`wasm` feature 已接线,目标构建验证留 CI 的 `wasm-check` job。

## 契约优先工作流(FSVDD,强制)

任何涉及磁盘格式、API 语义、状态流转、并发或错误的改动,必须按此顺序:

1. 先改 `docs/spec/contracts.md`(FC-Matrix,形式化约束的唯一真实数据源);
2. 再加/改测试,测试头部列出覆盖的 `I1–I30` 不变量与 `FC-*` 编号,函数注释引用编号;
3. 最后改实现。

禁止"先写实现后补契约",禁止契约漂移。新增业务逻辑至少登记一条 `FC-*` 且配 1:1 测试。
改算法/数据结构要同步维护 `contracts.md` §9 的 `FC-*-CPLX-*` 复杂度上界。
未实现测试的条目填 `待补`,不要编造测试文件名占位。

## 项目状态与兼容纪律(强制)

- 项目**尚未发布**:L1–L6 仍在 `feat/*` 分支开发,不存在任何外部旧库、旧段、旧
  MANIFEST、旧 WAL 或旧 API 消费者;开发中的磁盘格式一律视为**未发布**。
- **禁止为开发中的中间格式写兼容代码或兼容测试**:段/MANIFEST/WAL/hidx/msec 布局需要
  变更时,直接改当前定义并同步契约与测试;不保留"读旧开发格式"的读取分支、不写
  "旧库升级"回归、不为假想的旧读者升主/次版本或维护兼容矩阵。
- **动手前先看项目现状**:先读 `docs/DESIGN.md`、`docs/spec/contracts.md` 与 git 历史,
  确认功能/格式是否已经存在、是否已发布;禁止把"向后兼容/迁移/弃用"当成默认需求。
- 只有当某格式**已合并到 `main` 并对外发布**后,才需要兼容性设计;届时先在
  `contracts.md` 登记版本策略,再按该策略实现。

## 分层与代码边界

- L0→L6 只允许向下依赖;新代码必须落在正确的层,不能让下层依赖上层。
  **已文档化例外**:①门面/组合根(`memory::Builder`/`Mneme`)为装配需要可引用 `persist`、
  `index` 与 `life`(`Builder` 注入 `Store` 与 `IndexFactory`,`Mneme` 持有维护句柄与
  compaction 门面);②`src/quant/` 为**纯原语模块**(无 I/O、无锁、无全局态,依赖等级同 L0),
  L2 段编码与 L3 索引打分可直接复用。两条例外都不改变 L0→L6 的业务依赖方向。
- `src/core/`(L0)无 I/O、无全局状态、无锁,只有类型与纯函数。
- `unsafe` 只允许两处:`src/core/simd.rs` 的 arch 内联与 `src/persist/source.rs` 的 `MmapSource`
  (mmap 固有 unsafe);每处必须附 `// SAFETY:` 证明。
- L1 起公开 API 冻结;改签名需单独 RFC 并同步 `docs/design/16-api-reference.md`。
- `#![deny(missing_docs)]` + `#![deny(unsafe_op_in_unsafe_fn)]`:新增公开项必须有 rustdoc。

## 依赖与算法

- 依赖白名单:`Cargo.toml` 非 feature 直接依赖白名单上限 4 个(`serde`/`serde_json`/`thiserror`,以及 L2 的 `crc32fast`);L3 的 `memmap2` 随 feature `mmap`(默认开)引入,关后可回退 `FileSource`;L6 的 `half` 随 `quant-f16`、`tokio`(仅 `rt`)随 `async` 引入,均默认关。新增任何外部依赖都要在 PR 中论证必要性。
- HNSW、BM25、量化、bloom、compaction 调度、分词等复杂算法一律自研。

## 命令

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test                 # 单测 + 集成 + doctest
cargo doc --no-deps        # 公开项须 100% 文档覆盖

# 重门槛手动执行(不上 CI,需本机/专用 runner;默认 50_000×128 冒烟)
# 建库并行:默认块级串行 + 块内 HNSW 批并行(Builder::parallelism,0=可用核数);
# 块级并行经 MNEME_FLUSH_THREADS 显式开启(大内存/多核 runner 再试 2/4/8):
#   MNEME_FLUSH_THREADS=2 MNEME_HEAVY=1 cargo test --release --test cold_start -- --ignored
MNEME_HEAVY=1 cargo test --release --test cold_start --test heavy_gate -- --ignored
cargo test --release       # FC-GLOBAL-CPLX-001 复杂度操作计数
DURATION=3600 ./fuzz/scripts/run_long.sh    # fuzz 长跑(nightly + cargo-fuzz)
cargo mutants --no-shuffle --timeout 300    # 变异测试(mutants.toml;不阻断)
```

提交前按 fmt → clippy → test → doc 顺序跑。当前全绿,可作为基线。

## 尚未可用,别踩

- `[features]` 已定义:`mmap`(默认开,引入 `memmap2`)、`quant-f16`(引入 `half`)、
  `async`(引入 `tokio` 的 `rt`)、`fuzzing`(fuzz 专用解析入口,无依赖)、
  `encrypt`(引入 `aes-gcm` + `getrandom`)、`compress`(零依赖)、
  `compress-zstd`(引入 `zstd`)、`wasm`(关闭 mmap/后台线程;feature 已接线,
  目标构建验证留 CI 的 `wasm-check` job)。
- `cargo test --features async` / `--features quant-f16` / `--all-features` 均可跑;
  `AsyncNamespace` 的点读返回 owned `StoredRecord`(含 `RowId`)。
- `benches/hnsw.rs` 与 `benches/quant.rs`(criterion,dev-dependency)已引入;
  heavy 门槛(冷启动/吞吐/延迟)不上 CI,由本机/专用 runner 手动跑,规模可经
  `MNEME_HEAVY_ROWS`/`MNEME_HEAVY_DIM` 覆盖(默认 50_000×128 冒烟、回归建议
  100_000×128,吞吐/延迟门槛只在正式 1M×1536 下断言——待 ≥16GB 专用 runner);
  夜间趋势图与基线回归 >10% 阻断待基线入库。
- `fuzz/` 五目标骨架已搭起(独立 workspace,需 nightly + `cargo-fuzz`);
  `cargo test` 不编译 `fuzz/`,仓库内以 `src/fuzzing.rs` 冒烟单测兜底。
- 契约追溯门禁为 `tests/contract_traceability.rs`(随 `cargo test` 运行,校验契约↔测试双向映射);仓库内**没有** `xtask/` 或 `xtask check-contracts`,不要试图运行。
- `.gitlab-ci.yml` 只跑轻量两档 fast + middle(每次 push / MR;GitLab runner 已配置并通过首跑);heavy/fuzz/mutation 等重门槛不上 CI,由本机/专用 runner 手动执行(命令见上「命令」段),档位与触发定义见 `docs/design/14-testing.md §7`。

## 测试

- 契约测试:`tests/core_contracts.rs`(L0)、`tests/memory_contracts.rs`、
  `tests/query_contracts.rs`、`tests/model_contracts.rs`、`tests/life_contracts.rs`(L1)、
  `tests/persist_contracts.rs`(L2)、`tests/hnsw_contracts.rs`(L3)、`tests/l4_contracts.rs`(L4)、
  `tests/l5_contracts.rs`(L5)、`tests/l6_contracts.rs`(L6,含 async/f16 条件用例);
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
