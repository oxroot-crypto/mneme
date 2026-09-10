# 贡献指南

感谢参与 Mneme。本项目的核心约束写在设计文档里,提交前请先读:

- [docs/DESIGN.md](docs/DESIGN.md):分层地图、阅读路线、文档约定;
- [14 测试与验收](docs/design/14-testing.md):每层"完成"的验收标准;
- 各章末尾的**层边界契约**:本层向上暴露、向下依赖的精确接口。

## 基本规则

1. **单 crate、向下依赖**:`core` 之外禁止横向穿透;新代码必须落在正确的层
   (L0→L6),不得让下层依赖上层。
2. **依赖白名单**:新增任何外部依赖必须在 PR 描述中论证必要性,并给出隔离措施;
   复杂算法(HNSW、BM25、量化、bloom、compaction 调度、分词)一律自研,见
   [01 §5](docs/design/01-overview.md)。
3. **公开 API 冻结**:L1 起公开 API 签名冻结,变更需单独 RFC 并更新
   [16 API 参考](docs/design/16-api-reference.md)。
4. **不变量即测试锚点**:每个测试文件头部列出其覆盖的不变量编号(I1–I30)与
   [spec/contracts.md](docs/spec/contracts.md) 的 `FC-*` 编号,测试代码注释引用编号,
   防止"测了个寂寞";`tests/contract_traceability.rs` 在 `cargo test` 中校验契约↔测试双向映射。
5. **无 panic 契约**:L0 所有函数返回 `Result` 或数学上可证明不 panic;
   `unsafe` 仅允许出现在 `simd.rs` 的 arch 内联中。

## 提交前检查

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo doc --no-deps          # 公开项必须 100% 文档覆盖(#![deny(missing_docs)])
```

> `cargo test --features async`(async 门面等价性)等 feature 相关命令待对应层
> (含 `[features]` 定义)落地后加入本清单;feature 未定义时该命令会直接报错。

## 提交信息

遵循 [Conventional Commits](https://www.conventionalcommits.org/),首行 ≤ 72 字符:

```
<type>(<scope>): <祈使句描述>
```

- `type`:`feat` / `fix` / `docs` / `refactor` / `test` / `perf` / `build` / `ci` / `chore` 等;
- `scope`:受影响的层或模块(如 `core`、`persist`、`index`);
- 一次提交只做一件事,重构与功能不得混合;
- 正文说明"为什么"而非"改了什么"(diff 已说明后者),并引用相关章节或文档;
- 改动公开 API(参数、返回值、错误类型)时,须同步更新 rustdoc、所有调用点与测试,
  并在 `type` 后加 `!` 标注 breaking。

示例:

```
fix(persist): 拒绝未知类型的 WAL 帧

Per 04 §13,未知帧类型必须在回放时中止而非跳过。补充覆盖不变量 I2 的回归测试。
```

## 契约维护(FSVDD 强制)

本项目的业务逻辑遵循**契约优先、形式化约束驱动开发**:

1. 任何涉及磁盘格式、API 语义、状态流转、并发或错误的改动,**先改契约**
   [spec/contracts.md](docs/spec/contracts.md),再改测试,最后改代码;
2. 禁止"先写实现、后补契约";禁止契约漂移(改了代码/测试却不更新契约);
3. 新增业务逻辑必须至少登记一条 `FC-*` 约束,并给出 1:1 测试;
4. 涉及算法/数据结构的改动必须同步维护 [spec/contracts.md §9](docs/spec/contracts.md)
   的 `FC-*-CPLX-*` 复杂度契约:先更新上界,再改测试与实现;复杂度渐进退化视为破坏性变更;
5. 破坏性变更需在契约文件顶部"变更记录"标注版本与兼容性迁移约束;
6. 交付前自查:无孤儿实现、无失效契约、无孤立测试;
7. 证伪原则:每条 ERR/INV 契约须配备专项失败测试(放宽约束必有测试变红);
   机械化变异测试 `cargo-mutants`(配置见根目录 `mutants.toml`)列入 L2 阶段 CI 任务。

## 文档修改

- 设计文档改动请同步更新 [docs/DESIGN.md](docs/DESIGN.md) 的目录/约定与相关交叉引用;
- 数学公式统一用 KaTeX 书写(行内 `$...$`、块级 `$$...$$`),不再提供纯文本降级形式;
- 本地预览:`cargo install mdbook mdbook-mermaid && cargo install mdbook-katex --no-default-features --features duktape && mdbook serve`
  (Windows MSVC 无法编译 `mdbook-katex` 默认的 quick-js 后端,须改用 duktape 后端)。

## 报告问题

请在 [GitLab Issues](https://gitlab.oxroot.io/rustlib/mneme/-/issues) 提交,并附上:
版本、平台、最小复现、期望行为与实际行为。
