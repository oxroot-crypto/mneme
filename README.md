<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-dark.svg" />
  <img src="docs/assets/logo-light.svg" alt="Mneme logo" width="96" />
</picture>

# Mneme

**面向 AI Agent 超长期记忆的嵌入型向量存储引擎 · Rust · 无服务端**

[![License](https://img.shields.io/badge/license-Unlicense-blue?style=flat-square)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.93%2B-orange?style=flat-square)](Cargo.toml)
[![GitHub](https://img.shields.io/badge/GitHub-oxroot--crypto%2Fmneme-181717?style=flat-square&logo=github)](https://github.com/oxroot-crypto/mneme)

</div>

---

[English](README.en.md) | 简体中文

**Mneme**(μνήμη,古希腊语"记忆";记忆女神 Mnemosyne 的同源词)是一个纯 Rust 写的**嵌入型向量存储引擎**,面向 **AI Agent 的超长期记忆层**:进程内运行、无需服务端,数据经年累月增长而不失控。

LLM 每次对话结束就忘光上下文之外的一切。要跑上数月的 Agent,必须配一个能语义检索、能遗忘琐碎信息、能撑十年的外部记忆。Mneme 就是这样一个引擎。

**状态**:版本 `0.1.0`(发布准备中,尚未上线 crates.io)。L0–L6 已完整实现并通过形式化契约验收;1M×1536 性能门槛与 fuzz 长跑由本机/专用 runner 手动执行(不上 CI,正式门槛需 ≥16GB 专用 runner)。设计与验收标准见 [docs/DESIGN.md](docs/DESIGN.md)。

## 📑 目录

- [特性](#-特性)
- [架构](#-架构)
- [性能](#-性能)
- [安装](#-安装)
- [快速开始](#-快速开始)
- [用法](#-用法)
- [配置](#-配置)
- [文档](#-文档)
- [开发](#-开发)
- [贡献](#-贡献)
- [许可证](#-许可证)
- [致谢](#-致谢)

## ✨ 特性

- **嵌入型** — 一个本地目录就是数据库;无服务端、无网络、零运维。
- **Agent 记忆语义** — 命名空间、TTL、重要性、遗忘曲线、去重合并都是引擎级一等公民。
- **混合检索** — 向量 ANN 加 BM25 与过滤 DSL,支持 RRF / 加权融合。
- **记忆感知排序** — 相似度与新鲜度、重要度、访问频次、可信度可配置组合。
- **持久可靠** — WAL + 不可变段 + MANIFEST 原子提交,任意点掉电可恢复。
- **为长期而生** — 活跃段数 O(log N) 有界、内存占用有上界、磁盘格式可演化。
- **依赖极简** — 非可选直接依赖仅 4 个(`serde`、`serde_json`、`thiserror`、`crc32fast`;默认开 `mmap` 加 `memmap2`);复杂算法全部自研。

## 🏗️ 架构

引擎按 L0–L6 七层实现,每层都可独立交付;依赖只允许向下(L0 → L6)。

| 层 | 模块 | 提供 |
|---|---|---|
| L0 | `src/core/` | 标识类型、错误、距离度量、SIMD 点积、TopK 堆、位图、varint、元数据与配置类型 |
| L1 | `src/memory/` | 内存表、暴力检索、过滤 AST、去重、生命周期与公开 API |
| L2 | `src/persist/` | WAL、不可变段文件、MANIFEST、崩溃恢复、增量段 flush |
| L3 | `src/index/` | 自研 HNSW(过滤感知)、`hidx` 持久化、mmap 段读取 |
| L4 | `src/query/` | 过滤 DSL、查询计划器、BM25、RRF / 加权融合、执行管线 |
| L5 | `src/life/` | size-tiered compaction、后台维护、TTL 剪枝、快照与备份 |
| L6 | `src/quant/` | i8/f16 量化副本、两阶段精排、`async` 下的 `AsyncNamespace` |

能力明细:

| 能力 | 说明 |
|---|---|
| 向量检索 | 余弦 / 点积 / 欧氏;自研 HNSW,小段自动暴力扫描 |
| 混合检索 | 过滤 DSL + BM25 + RRF / 加权融合 + 结果去重 + MMR 多样性 |
| 记忆感知排序 | 相似度与新鲜度、重要度、访问频次、可信度可配置组合;联想扩展 |
| 记忆模型 | 关系图、双时态 `as_of` 时间旅行(历史默认永久保留)、`supersede` 信念修订、来源/可信度、记忆沉淀 |
| 记忆生命周期 | TTL 两阶段过期、指数遗忘曲线、访问强化、命名空间隔离(自动遗忘默认关闭) |
| 持久化 | WAL + 不可变段 + MANIFEST 原子提交;删除不复活 |
| 超长期 | size-tiered compaction;写放大 O(log N);活跃段数有界 |
| 量化 | i8 / f16 量化副本 + 两阶段精排;查询带宽 i8 ÷4、f16 ÷2;建段抽样不达标自动回退 f32 |
| 存储安全 | AES-256-GCM 静态加密与密钥轮换(`encrypt`)、文本/元数据压缩(`compress` / `compress-zstd`,无收益回退原文) |
| 部署形态 | 多进程只读共享(`Mneme::reload`)、`Storage`/`FsStorage`/`MemStorage` 后端(WASM/边缘)、`Observer` 事件钩子 |

## 📊 性能

> 50 000 行 × 128 维 · 1 000 条查询 · top-10 · L2² · `M=16` / `ef_construction=200` ·
> 4 核 Intel Xeon 8255C。多轮轮转交错取中位数(轮间极差 ±3–8%);
> **Mneme 构建为真实建库路径**(`insert_batch` + `flush`:WAL + 段文件 + MANIFEST + 图落盘),
> 其余引擎为纯内存建图。完整方法、召回全表与原始数据见
> [`comparison/README.md`](comparison/README.md)。

### 建库耗时(4 线程,越短越好)

```text
usearch         ██ 3.76 s
hnsw_stable     ██████ 8.66 s
mneme i8        ███████ 10.52 s
hnsw_rs         ███████ 10.61 s
mneme (f32)     ███████ 10.85 s
instant_dist    ██████████████████████████████████████████████ 69.32 s
```

### 单线程查询延迟 P50(ef=128,越短越好)

```text
usearch         █████████████████ 189 µs
mneme i8        █████████████████ 193 µs
mneme (f32)     ███████████████████████████ 295 µs
hnsw_stable     ██████████████████████████████████ 377 µs
hnsw_rs         ████████████████████████████████████████ 441 µs
```

### 4 线程总吞吐(ef=128,越高越好)

```text
usearch         ████████████████████████████████████████ 17 020 QPS
mneme i8        ████████████████████████████████ 13 699 QPS
mneme (f32)     ████████████████████████ 10 095 QPS
hnsw_rs         █████████████ 5 599 QPS
hnsw_stable     █████████████ 5 502 QPS
```

### 关键数字对照

| 引擎 | ef | Recall@10 | P50 | 4 线程 QPS | RSS 增量 |
| --- | ---: | ---: | ---: | ---: | ---: |
| usearch | 128 | 0.9996 | 189 µs | 17 020 | 37.2 MiB |
| **Mneme(i8,按需开启)** | 128 | 0.9980 | **193 µs** | **13 699** | 103.7 MiB |
| **Mneme(默认 f32)** | 128 | 0.9980 | 295 µs | 10 095 | 97.3 MiB |
| hnsw_stable | 128 | 1.0000 | 377 µs | 5 502 | 39.4 MiB |
| hnsw_rs | 128 | 0.9799 | 441 µs | 5 599 | 134.3 MiB |
| instant_distance | 64 | 0.9999 | 296 µs | 12 453 | 67.4 MiB |

### 怎么读这些数字

- **构建**:Mneme 10.9 s 与 hnsw_rs(10.6 s)同档,慢于 hnsw_stable(8.7 s)与
  usearch(3.8 s);但 Mneme 是唯一把 WAL、不可变段、MANIFEST 与图文件全部写盘、
  可在任意点崩溃恢复的实现。其中 `insert_batch`(WAL 追加)仅 0.45 s,
  其余是 `flush` 内的建图与编码。
- **查询**:默认 f32 档与 hnsw_stable 持平(295 µs vs 377 µs);按需开启 i8 量化副本
  (`.quantization(VectorFormat::I8Rescored)`)后 P50 193 µs,与最快的 C++ 实现
  usearch 基本持平,召回仅降 ≤0.001,4 线程吞吐 +36%。
- **召回**:第一梯队——ef=128 时 0.9980,ef=256 时 1.0000;hnsw_rs 落后约 2 个百分点。
- **纯内存形态**:`Builder` 不设 `path` 即纯内存库;`flush()` 后同样建 HNSW 内存段
  (不写盘、不做量化副本),查询与召回同持久库一致,构建约 −7%(省 WAL 与段编码)、
  RSS 约 −17%。未 `flush()` 的纯内存库走精确暴力扫描(20k×128 P50 ≈ 0.7 ms),
  大规模内存库请先 `flush()`。
- 均匀随机高维数据(ANN 最困难集合)下所有引擎召回按预期塌缩,而 Mneme 每档召回
  最高,细节见 [`comparison/README.md`](comparison/README.md)。

## 📦 安装

### 环境要求

| 依赖 | 最低版本 | 说明 |
|---|---|---|
| Rust | 1.93(edition 2024) | `Cargo.toml` 的 `rust-version` 声明 |
| C 工具链 | — | 仅 feature `compress-zstd` 需要(`zstd-sys` 编译 C 源码) |

### 添加依赖

当前版本 `0.1.0`,尚未发布到 crates.io:请按路径或 Git 引入;发布后照常写 `mneme = "0.1"` 即可。

```toml
[dependencies]
mneme = { path = "../mneme" }                              # 本地克隆后按路径引入
# mneme = { git = "https://github.com/oxroot-crypto/mneme" }  # 或按 Git 引入
# mneme = "0.1"                                           # 发布到 crates.io 之后
```

## 🚀 快速开始

### 1. 建一个 demo 工程

```bash
git clone https://github.com/oxroot-crypto/mneme.git
cargo new agent-memory && cd agent-memory
cargo add --path ../mneme mneme     # 未发布到 crates.io,按路径引入
```

### 2. 写入 `src/main.rs`

```rust
use mneme::{FsyncPolicy, Metric, Mneme, Record};
use std::time::Duration;

fn main() -> mneme::Result<()> {
    // 打开(或新建)一个本地目录作为记忆库
    let db = Mneme::builder()
        .path("./agent_memory")                       // 省略则 = 纯内存库(易失)
        .dimension(4)                                 // 新建必填;实际用嵌入模型维度(如 1536)
        .metric(Metric::Cosine)                       // 默认 Cosine
        .fsync(FsyncPolicy::Batched(Duration::from_millis(20)))
        .build()?;

    let ns = db.namespace("agent-42/profile");        // 分层命名空间

    // 记住两件事(向量由宿主侧的嵌入模型产生)
    ns.insert(
        Record::new(vec![1.0, 0.0, 0.0, 0.0])
            .key("pref.theme")                        // 可选外部键;重复即 upsert
            .text("the user prefers dark mode")       // 可选;启用 BM25 与文本去重
            .importance(0.8),
    )?;
    ns.insert(
        Record::new(vec![0.0, 1.0, 0.0, 0.0])
            .key("pref.os")
            .text("the user runs macOS")
            .importance(0.6),
    )?;

    // 想起相关的记忆
    let hits = ns
        .search()
        .vector(&[1.0, 0.0, 0.0, 0.0])
        .top_k(2)
        .execute()?;
    for hit in &hits {
        let key = hit.key.as_ref().map(|k| k.as_str()).unwrap_or("-");
        println!("{key} {:.3}", hit.score);
    }

    db.close()?;                                      // 优雅关闭:flush + 释放文件锁
    Ok(())
}
```

### 3. 运行

```bash
cargo run
```

```text
pref.theme 1.000
pref.os 0.000
```

完整 API 语义、配置项、错误处理与备份恢复见 [16 公开 API 与运维参考](docs/design/16-api-reference.md)。

## 📖 用法

以下片段承接快速开始里的 `db` 与 `ns`。

### 混合检索:向量 + 关键词 + 过滤 + 综合打分

```rust
use mneme::{filter, Diversity, Scoring};

// q 由宿主侧的嵌入模型产生
let hits = ns
    .search()
    .vector(&q)                                      // 向量通道
    .text("dark mode")                                // BM25 通道;融合默认 RRF(k = 60)
    .filter(filter!(r#"kind == "preference""#))      // 写死字面量用 filter!;运行时输入用 Expr::from_str
    .score(Scoring { w_recency: 0.2, w_importance: 0.3, ..Scoring::default() })
    .diversify(Diversity::Mmr { lambda: 0.7 })       // MMR 多样性
    .top_k(10)
    .execute()?;
// hits 按综合分降序;Hit 不含向量,需要时用 ns.get_vector(hit.rowid)
```

### 生命周期:TTL、强化与主动遗忘

```rust
use mneme::{filter, Retention};
use std::time::Duration;

// TTL:写入相对时长,引擎换算成绝对 expires_at
ns.insert(
    Record::new(vec![0.0, 0.0, 1.0, 0.0])
        .key("scratch-1")
        .metadata(mneme::json!({"kind": "scratch"}))
        .ttl(Duration::from_secs(7 * 86400)),
)?;
ns.touch("pref.theme", Some(0.1))?;                  // 访问强化
let forgotten = ns.forget(filter!(r#"kind == "scratch""#))?;   // 打墓碑;物理回收交给 compaction

// 遗忘曲线:半衰期 + 重要性下限 + 保护白名单(后台自动遗忘默认关闭)
let report = ns.retain(
    Retention::new()
        .half_life(Duration::from_secs(14 * 86400))
        .min_importance(0.2)
        .protect(filter!(r#"kind == "decision""#)),
)?;
// forgotten == 1,report.forgotten == 0
```

### 时间旅行与信念修订

```rust
// 事务时间快照:先取句柄,再往后写,句柄内视图不变
let t1 = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_millis() as i64;
std::thread::sleep(Duration::from_millis(5));
ns.insert(Record::new(vec![0.0, 1.0, 0.0, 0.0]).key("pref.os"))?;

let old = db.as_of(t1)?;                             // 快照句柄,可反复读
let old_ns = old.namespace("agent-42/profile");
let old_hits = old_ns.search().vector(&[1.0, 0.0, 0.0, 0.0]).top_k(10).execute()?;
assert_eq!(old_hits.len(), 1);                       // t1 时只有 pref.theme

// 信念修订:旧版本 valid_to 闭合;历史默认永久保留
ns.supersede(
    "pref.theme",
    Record::new(vec![0.0, 0.0, 0.0, 1.0]).valid_from(1_700_000_000_000),
)?;
```

### 静态加密与压缩(feature)

```toml
[dependencies]
mneme = { path = "../mneme", features = ["encrypt", "compress"] }
```

```rust
use std::sync::Arc;
use mneme::{Cipher, Compression, CryptoKey, Encryption, KeyId, Keyring};

// Keyring 是内置的内存密钥环;生产可换成环境变量、keychain 或 KMS 实现
let keyring = Arc::new(Keyring::new(KeyId(1), CryptoKey::generate()?));
let db = Mneme::builder()
    .path("./encrypted_memory")
    .dimension(1536)
    .encryption(Some(Encryption { provider: keyring.clone(), cipher: Cipher::Aes256Gcm }))
    .compression(Compression::Lz4)
    .build()?;
    let new_key = db.rotate_encryption_key()?;           // 密钥轮换:全量段重写
```

### 端到端示例:交互式记忆库,接第三方嵌入 API(OpenAI 协议)

`examples/memory` 是一个交互式 REPL:经 OpenAI 协议调用官方或任意兼容端点
(OpenRouter / vLLM / Ollama 的 `/v1` / LM Studio),把 `/add` 的文本嵌入后写进 Mneme,
用 `/search` 做向量 + BM25 混合检索;`EmbeddingProvider` trait 预留了换协议的扩展点。

```bash
# 仓库根放 .env(已 gitignore)或直接 export:EXAMPLE_EMBEDDING_API_KEY=sk-...
cargo run --example memory
# 兼容端点加两行环境变量:MNEME_EMBEDDING_BASE_URL / MNEME_EMBEDDING_MODEL
#
# REPL 命令:/add、/search、/get、/list、/touch、/delete、/help、/quit
```

示例内置一套离线路径用例(mock 端点覆盖响应乱序、401、畸形 JSON、断连、维度冲突、
批写原子性、持久化重开、命令解析等):`cargo test --example memory`。

## ⚙️ 配置

所有旋钮都有默认值;先默认跑通,再根据 `db.stats()` 调优。完整配置表(含 `HnswParams`、`CompactionPolicy`、`Tuning` 与 `Limits`)见 [16 §2](docs/design/16-api-reference.md)。

| 选项 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `.path` | `impl AsRef<Path>` | 无(纯内存) | 一个目录 = 一个库 |
| `.dimension` | `u32` | 新建必填 | 1..=65536,建库后不可改 |
| `.metric` | `Metric` | `Cosine` | `Cosine` / `Dot` / `Euclidean` |
| `.fsync` | `FsyncPolicy` | `Batched(20ms)` | `Always` / `Batched` / `OnFlush` / `Never` |
| `.insert_mode` | `InsertMode` | `Upsert` | 同 key 行为:`Upsert` / `RejectDuplicate` |
| `.dedup` | `Dedup` | `Off` | `Off` / `Reject` / `Replace` / `KeepBoth` / `Merge` |
| `.dedup_threshold` | `f32` | `0.95` | 近似去重阈值,统一按余弦口径 |
| `.quantization` | `VectorFormat` | `F32` | `F32` / `F16` / `I8Rescored`(仅持久库) |
| `.hnsw` | `HnswParams` | `m=16, m0=32, ef_construction=200, ef_search=64` | 越界建库即拒绝 |
| `.build_precision` | `BuildPrecision` | `Hybrid` | 遍历走临时 i8 码流、选邻按 f32 精排;`F32` 为全精确原行为 |
| `.compaction` | `CompactionPolicy` | 见 [16 §2](docs/design/16-api-reference.md) | `tier_ratio=4`、`dead_ratio=0.25`、`segment_rows=8192`、`history_horizon=None`(永久) |
| `.retention` | `Option<Retention>` | `None` | 不显式设置则后台自动遗忘关闭 |
| `.access_flush_interval` | `Duration` | `30s` | 访问计数批量落盘周期 |
| `.compression` | `Compression` | `None` | `Lz4`(feature `compress`)/ `Zstd`(`compress-zstd`) |
| `.encryption` | `Option<Encryption>` | `None` | 静态加密(需 feature `encrypt`) |
| `.storage` | `Arc<dyn Storage>` | `FsStorage` | 自定义后端(如 `MemStorage`) |
| `.read_only` | `bool` | `false` | 多进程只读共享 |
| `.read_only_probe_interval` | `Duration` | `1s` | 只读实例探测新 MANIFEST 的周期;`ZERO` = 关闭 |
| `.relation_index` | `RelationIndex` | `Outgoing` | `Outgoing` / `Both` |
| `.parallelism` | `usize` | `0`(自动) | 并行线程数;0 = 可用核数 |
| `.maintenance` | `bool` | `true` | `false` 闸住后台维护线程(批量导入用) |
| `.observer` | `Arc<dyn Observer>` | 无 | Query / Write / Flush / Compaction / Error 事件钩子 |
| `.verify_on_open` | `bool` | `false` | 打开即全量校验段 payload CRC(慢) |
| `.fail_fast_on_corruption` | `bool` | `false` | 遇损坏段拒绝启动,而非隔离跳过 |

### Cargo feature

| feature | 默认 | 说明 |
|---|---|---|
| `mmap` | 开 | 段文件 mmap 零拷贝读;关闭后走 `Read + Seek` 兜底 |
| `async` | 关 | `AsyncNamespace` async 门面(`spawn_blocking`);核心零 tokio |
| `quant-f16` | 关 | f16 量化副本;关闭时 `F16` 于构造期返回 `Unsupported` |
| `fuzzing` | 关 | `fuzz/` 目标的解析入口,不改变运行时行为 |
| `encrypt` | 关 | 段/WAL/MANIFEST 的 AES-256-GCM 信封加密;密钥轮换见 `Mneme::rotate_encryption_key` |
| `compress` | 关 | `text`/`meta`/`provenance` 压缩(内置 LZ4 风格 codec,零依赖) |
| `compress-zstd` | 关 | 经 `zstd` 的可选更强压缩 |
| `wasm` | 关 | 关闭 mmap 与后台线程,面向 WASM 目标;配合 `MemStorage` |

## 📚 文档

| 文档 | 内容 |
|---|---|
| [docs/DESIGN.md](docs/DESIGN.md) | 总入口:分层地图、阅读路线、文档约定 |
| [开发者指南](docs/guide.md) | **面向使用方**:从引入依赖到上生产的完整路径 |
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
| [comparison/README.md](comparison/README.md) | 与 usearch / hnsw_rs / hnsw-stable / instant-distance 的横向基准(方法学、全表、复现命令) |
| [CHANGELOG.md](CHANGELOG.md) | 变更记录(Keep a Changelog 格式) |
| [rust/README.md](docs/rust/README.md) | **Rust 零基础教学**(11 章):以本仓库源码为教材 |

设计文档目前只有中文版。

## 🧪 开发

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test                       # 单测 + 集成测试 + doctest
cargo doc --no-deps              # rustdoc 门禁:#![deny(missing_docs)]
cargo bench                      # criterion 基准(benches/hnsw.rs、benches/quant.rs)
```

feature 相关测试:

```bash
cargo test --features async
cargo test --features quant-f16
cargo test --all-features
```

重门槛(手动执行,不上 CI;正式 1M×1536 门槛需 ≥16GB 专用 runner):

```bash
MNEME_HEAVY=1 cargo test --release --test cold_start --test heavy_gate -- --ignored
cargo test --release                       # FC-GLOBAL-CPLX-001 复杂度操作计数
DURATION=3600 ./fuzz/scripts/run_long.sh   # fuzz 长跑(nightly + cargo-fuzz)
cargo mutants --no-shuffle --timeout 300   # 变异测试(mutants.toml)
```

规模可经 `MNEME_HEAVY_ROWS` / `MNEME_HEAVY_DIM` 覆盖;`MNEME_FLUSH_CHUNK_ROWS` / `MNEME_FLUSH_THREADS` 调节批量导入的切块与块级并行度。

用 [mdBook](https://rust-lang.github.io/mdBook/) 构建文档站点:

```bash
cargo install mdbook mdbook-mermaid
# Windows MSVC 无法编译 mdbook-katex 默认的 quick-js 后端,必须改用 duktape 后端:
cargo install mdbook-katex --no-default-features --features duktape
mdbook-mermaid install .   # 首次运行:复制 mermaid 资源并补全 book.toml
mdbook serve               # 本地预览 http://localhost:3000
mdbook build               # 输出到 book/
```

项目 CI(仓库根 `.gitlab-ci.yml`)只跑轻量两档:`fast` 随每次 push(fmt + clippy + 单测 + feature 矩阵 + rustdoc 门禁 + wasm32 构建检查),`middle` 随 PR(全量 L2–L6 契约集成,加 f16/encrypt 跨 feature 矩阵)。重门槛、fuzz 长跑与变异测试仍由本机/专用 runner 手动执行。

MSRV 为 **1.93**(edition 2024),在 `Cargo.toml` 中声明;CI 以 `rust:1.93` 镜像构建与测试。

## 🤝 贡献

见 [CONTRIBUTING.md](CONTRIBUTING.md)。要点:

1. 契约优先:涉及磁盘格式、API 语义、状态流转、并发或错误的改动,先改 [spec/contracts.md](docs/spec/contracts.md),再改测试,最后改代码。
2. 新增外部依赖必须在 PR 中论证;HNSW、BM25、量化、bloom、compaction、分词一律自研。
3. 提交信息遵循 [Conventional Commits](https://www.conventionalcommits.org/),首行 ≤ 72 字符。

问题与 PR 入口:[github.com/oxroot-crypto/mneme](https://github.com/oxroot-crypto/mneme/issues)。

## 📄 许可证

[The Unlicense](LICENSE) —— 释放到公共领域。

## 🙏 致谢

- 本仓库的代码与文档**完全**由 AI 生成:**deepseek-v4.1-flash** 与 **glm-5.3-flash**;项目名称 **Mneme** 由 **gemini-3.8-flash** 拟定;详见 [DISCLOSURE.md](DISCLOSURE.md)。
- 名字取自希腊语 **μνήμη**(*mnḗmē*,记忆),也是记忆女神 Mnemosyne 的同源词。
- 可选能力依赖几个小而专注的 crate:[`memmap2`](https://crates.io/crates/memmap2)、[`half`](https://crates.io/crates/half)、[`tokio`](https://crates.io/crates/tokio)(仅 `rt`)、[`aes-gcm`](https://crates.io/crates/aes-gcm)、[`getrandom`](https://crates.io/crates/getrandom) 与 [`zstd`](https://crates.io/crates/zstd)。
- 测试与基准使用 [`proptest`](https://crates.io/crates/proptest)、[`tempfile`](https://crates.io/crates/tempfile) 与 [`criterion`](https://crates.io/crates/criterion);端到端示例(`examples/memory`)经 [`async-openai`](https://crates.io/crates/async-openai) 调用嵌入 API(均为 dev-dependency)。
