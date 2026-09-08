# 贡献指南

感谢参与 Mneme。本项目的核心约束写在设计文档里,提交前请先读:

- [docs/DESIGN.md](docs/DESIGN.md):分层地图、阅读路线、文档约定;
- [09 测试与验收](docs/design/09-testing.md):每层"完成"的验收标准;
- 各章末尾的**层边界契约**:本层向上暴露、向下依赖的精确接口。

## 基本规则

1. **单 crate、向下依赖**:`core` 之外禁止横向穿透;新代码必须落在正确的层
   (L0→L6),不得让下层依赖上层。
2. **依赖白名单**:新增任何外部依赖必须在 PR 描述中论证必要性,并给出隔离措施;
   复杂算法(HNSW、BM25、量化、bloom、compaction 调度、分词)一律自研,见
   [01 §5](docs/design/01-overview.md)。
3. **公开 API 冻结**:L1 起公开 API 签名冻结,变更需单独 RFC 并更新
   [11 API 参考](docs/design/11-api-reference.md)。
4. **不变量即测试锚点**:每个测试文件头部列出其覆盖的不变量编号(I1–I18),
   测试代码注释引用编号,防止"测了个寂寞"。
5. **无 panic 契约**:L0 所有函数返回 `Result` 或数学上可证明不 panic;
   `unsafe` 仅允许出现在 `simd.rs` 的 arch 内联中。

## 提交前检查

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --features async
cargo doc --no-deps          # 公开项必须 100% 文档覆盖(#[deny(missing_docs)])
```

## 提交信息

使用祈使句、首行 ≤ 72 字符,并在正文引用相关章节或文档,例如:

```
persist: reject WAL frames with unknown type

Per 04 §13, an unrecognized frame type must abort replay rather than be
skipped. Adds regression test covering invariant I2.
```

## 文档修改

- 设计文档改动请同步更新 [docs/DESIGN.md](docs/DESIGN.md) 的目录/约定与相关交叉引用;
- 数学公式遵循"双写"约定:KaTeX 与纯文本各给一份;
- 本地预览:`cargo install mdbook mdbook-katex mdbook-mermaid && mdbook serve`。

## 报告问题

请在 [GitLab Issues](https://gitlab.oxroot.io/rustlib/mneme/-/issues) 提交,并附上:
版本、平台、最小复现、期望行为与实际行为。
