# Mneme

> **Mneme**(μνήμη,古希腊语"记忆";记忆女神 Mnemosyne 的同源词)是一个纯 Rust 编写的**嵌入型向量存储引擎**,
> 专为 **AI Agent 的超长期记忆层**设计:进程内运行、无需服务端、数据经年累月增长而不失控。

**状态**:开发中(尚未发布到 crates.io)。**L0 原语层(`src/core/`)与 L1 内存引擎(`src/memory/`)**已实现
并通过形式化契约验收,后续按 L2→L6 逐层推进,每层完成时都是一个可独立交付的完整产品。
设计与验收标准见 [docs/DESIGN.md](docs/DESIGN.md)。

---

## 它解决什么问题

LLM 每次对话结束就"忘光"上下文之外的一切。要让 Agent 长期工作,必须给它配一个外部记忆系统:
能**语义检索**(问"他偏好什么界面风格?"能想起"用户喜欢深色模式")、能**自动遗忘**琐碎信息、
并且**跑十年也不失控**。Mneme 就是这样一个引擎:

- **嵌入型(embedded)**:像 SQLite 一样链接进你的程序,数据就是本地一个目录,无服务、无网络、零运维;
- **Agent 记忆特化**:命名空间隔离、TTL/重要性/遗忘曲线、去重合并、混合检索都是引擎级一等公民;
- **超长期**:活跃段数 O(log N) 有界、冷数据自动沉磁盘、内存占用有上界、文件格式可演化。

## 特性

| 能力 | 说明 |
|---|---|
| 向量检索 | 余弦 / 点积 / 欧氏;自研 HNSW(过滤感知),小数据自动暴力扫描 |
| 混合检索 | 过滤 DSL + BM25 关键词 + RRF/加权融合 + 去重 + MMR 多样性 |
| **记忆感知排序** | 相似度 + 新鲜度 + 重要度 + 访问频次 + 可信度可配置组合;联想扩展 |
| **记忆模型** | 关系图(联想检索)、双时态 `as_of` 时间旅行(历史版本默认永久保留,可配保留期)、`supersede` 信念修订、来源/可信度、记忆沉淀 |
| 记忆生命周期 | TTL 两阶段过期、指数遗忘曲线、访问强化、命名空间隔离(自动遗忘默认关闭) |
| 持久化 | WAL + 不可变段 + delta 覆盖区 + MANIFEST 原子提交,任意点掉电可恢复、删除不复活 |
| 超长期 | size-tiered compaction,写放大 O(log N)、活跃段数有界 |
| 量化 | i8 / f16 量化副本 + 两阶段重打分,查询带宽 i8 ÷4 / f16 ÷2(f32 原向量保留供精排,故磁盘不缩减) |
| **存储安全**(可选) | AES-256-GCM 静态加密、文本/元数据压缩 |
| **部署形态** | 多进程只读共享;`Storage` 抽象支持 WASM/边缘适配;可观测事件钩子 |
| 依赖极简 | 非 feature 强依赖 4 个小 crate(默认开 `mmap` 共 5 个;均为直接依赖,`serde_json` 另带入 itoa/ryu/memchr 等极少数传递依赖),复杂算法全部自研;加密/压缩均为可选 feature |

## 安装

> 尚未发布,以下为发布后的目标用法。

```toml
[dependencies]
mneme = "0.1"
```

## 快速开始

```rust
use mneme::{FsyncPolicy, Metric, Mneme, Record};
use std::time::Duration;

fn main() -> mneme::Result<()> {
    // 打开(或新建)一个本地目录作为记忆库
    let db = Mneme::builder()
        .path("./agent_memory")                        // 省略则 = 纯内存(易失)
        .dimension(1536)                               // 必填;首次建库后不可改
        .metric(Metric::Cosine)                        // 默认 Cosine
        .fsync(FsyncPolicy::Batched(Duration::from_millis(20)))
        .build()?;

    let ns = db.namespace("agent-42/session-88");      // 分层命名空间

    // 记住一件事
    let vector = vec![0.0f32; 1536];                   // 由外部嵌入模型产生
    ns.insert(
        Record::new(vector)
            .key("mem_001")                            // 可选外部键;重复即 upsert
            .text("用户偏好深色模式")                    // 可选;启用 BM25 与文本去重
            .ttl(Duration::from_secs(30 * 86400)),     // 可选;到期自动遗忘
    )?;

    // 想起相关的记忆
    let query = vec![0.0f32; 1536];
    let hits = ns.search().vector(&query).top_k(10).execute()?;
    for hit in hits {
        println!("{:?} {}", hit.key, hit.score);
    }

    db.close()?;                                       // 优雅关闭:flush + 释放文件锁
    Ok(())
}
```

完整 API 语义、配置项、错误处理与备份恢复见
[16 公开 API 与运维参考](docs/design/16-api-reference.md)。

## Feature 开关

| feature | 默认 | 说明 |
|---|---|---|
| `mmap` | ✅ 开 | 段文件 mmap 零拷贝读;关闭后走 `Read + Seek` 兜底 |
| `async` | ❌ 关 | 提供 `insert().await` 等 async 门面(`spawn_blocking` 薄包装) |
| `quant-f16` | ❌ 关 | f16 量化副本;关闭时只有 f32 / i8 |
| `encrypt` | ❌ 关 | AES-256-GCM 静态加密 |
| `compress` | ❌ 关 | 文本/元数据压缩(内置 LZ4 风格 codec) |
| `compress-zstd` | ❌ 关 | 可选更强压缩(引入 `zstd`) |
| `wasm` | ❌ 关 | 关闭 mmap/线程并行,WASM 目标 |

## 文档

| 文档 | 内容 |
|---|---|
| [docs/DESIGN.md](docs/DESIGN.md) | 总入口:分层地图、阅读路线、文档约定 |
| [00 零基础篇](docs/design/00-fundamentals.md) | 嵌入向量、相似度、ANN、WAL/MVCC 等全部前置概念 |
| [01 总览](docs/design/01-overview.md) | 定位、架构、依赖白名单、公开 API 速览 |
| [02–08 各层设计](docs/design/02-l0-core.md) | L0 原语 → L6 量化,含完整数学推导 |
| [14 测试与验收](docs/design/14-testing.md) | 崩溃注入、召回属性测试、基准、fuzz、长跑 |
| [15 术语表](docs/design/15-glossary.md) | 中英对照、符号表、复杂度速查 |
| [16 API 与运维参考](docs/design/16-api-reference.md) | 完整 API、配置总表、错误/重试、线程安全、备份恢复 |
| [09 记忆模型](docs/design/09-memory-model.md) | 关系图、双时态、来源/可信度、记忆沉淀 |
| [10 记忆感知排序](docs/design/10-scoring.md) | 综合打分、联想扩展、反馈闭环、MMR |
| [11 存储安全与压缩](docs/design/11-security-storage.md) | 静态加密、文本/元数据压缩 |
| [12 部署形态](docs/design/12-deployment.md) | 多进程只读、WASM 适配、可观测 |
| [13 记忆模式手册](docs/design/13-cookbook.md) | Agent 记忆配方(可直接照抄) |
| [spec/contracts.md](docs/spec/contracts.md) | 形式化契约矩阵(FC-Matrix) |
| [rust/README.md](docs/rust/README.md) | **Rust 零基础教学**(10 章):以 mneme 源码为教材,面向没有 Rust 基础的开发者 |

## 构建文档站点

设计文档用 [mdBook](https://rust-lang.github.io/mdBook/) 组织,KaTeX 与 Mermaid 由预处理器渲染:

```bash
cargo install mdbook mdbook-mermaid
# Windows MSVC 无法编译 mdbook-katex 默认的 quick-js 后端,必须改用 duktape 后端:
cargo install mdbook-katex --no-default-features --features duktape
mdbook-mermaid install .   # 首次运行:复制 mermaid 资源并写入 book.toml
mdbook serve               # 本地预览 http://localhost:3000
mdbook build               # 输出到 book/
```

## 开发与测试

> 当前已实现 L0 原语层与 L1 内存引擎,下列命令即可运行;`cargo test --features async`(async 门面
> 等价性)等 feature 相关命令待对应层(含 `[features]` 定义)落地后加入。

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test                       # 单测 + 集成测试 + doctest
cargo doc --no-deps              # 公开项 100% 文档覆盖(#![deny(missing_docs)])
cargo bench                      # criterion 基准(L3 起)
```

CI 分 fast / middle / heavy / fuzz 四档,矩阵覆盖 Linux(x86_64/aarch64)与
Windows(x86_64),详见 [14 §7](docs/design/14-testing.md)。

## MSRV

最低支持的 Rust 版本(MSRV)在 `Cargo.toml` 的 `rust-version` 中声明。

## 贡献

见 [CONTRIBUTING.md](CONTRIBUTING.md)。新增任何外部依赖必须在 PR 中论证,
复杂算法(HNSW、BM25、量化、bloom、compaction、分词)一律自研。

## 许可证

[The Unlicense](LICENSE) —— 释放到公共领域。
