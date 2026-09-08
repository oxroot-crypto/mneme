# 01 总览:定位、架构与分层

> **本章目标**:建立对 Mneme 的整体认知——它做什么、不做什么、由什么组成、按什么顺序建造。
> **前置阅读**:[00 零基础篇](00-fundamentals.md)(至少 §1–§5)。
> **本章你将学到**:设计目标表 → 总体架构 → L0–L6 渐进式分层 → 依赖白名单 → 公开 API 清单。

---

## 1. 定位

Mneme 是一个**纯 Rust 的进程内嵌入式向量存储库**,面向 **AI Agent 的超长期记忆层**:

- **嵌入型**:像 SQLite 一样作为库链接进宿主程序,数据就是本地一个目录,无服务、无网络、零运维;
- **Agent 记忆特化**:命名空间隔离、TTL/重要性/遗忘曲线、去重合并、混合检索都是引擎级一等公民,
  而不是应用层 hack;
- **超长期**:目标是"一个 Agent 用十年"——数据量增长段数有界、冷数据自动沉磁盘、
  内存占用有上界、老数据格式可演化。

### 1.1 设计目标

| 目标 | 量化标准 | 兑现章节 |
|---|---|---|
| 嵌入式零运维 | 单进程独占(文件锁);`cargo add mneme` 即用 | [01](01-overview.md) |
| 超长期不失控 | 活跃段数 O(log N) 有界;1M 条冷启动打开 < 1s;RSS 有上界 | [07](07-l5-life.md) |
| 高召回低延迟 | 1M×1536 维 P99 查询 < 10ms(量化后),Recall@10 ≥ 0.95 | [05](05-l3-hnsw.md) + [08](08-l6-quant.md) |
| 吞吐 | 批量插入 ≥ 50k 向量/秒 | [09](09-testing.md) |
| 崩溃安全 | 任意时刻掉电:已确认写入不丢、不出现半写数据 | [04](04-l2-persist.md) + [09](09-testing.md) |
| 最小依赖 | 非 feature 强依赖 4 个小 crate;默认开启 `mmap` 时共 5 个,多出的 1 个(`memmap2`)可经 feature 关闭;复杂算法全部自研 | [01 §5](01-overview.md) |

### 1.2 非目标(v1 明确不做)

- 多进程并发读写(单进程独占;多进程需求留给未来);
- 分布式、副本、分片;
- 内置 embedding 推理(Mneme 只存向量,嵌入由宿主调用外部模型产生);
- SQL 或查询语言完整实现(只有记忆检索所需的过滤 DSL);
- 静态加密、访问控制与多租户鉴权:数据以明文存放在本地目录,安全边界 = 宿主进程的
  文件系统权限;需要加密时由宿主对目录整体加密(如全盘/文件系统层),Mneme 不内置密钥管理;
- 文本/元数据压缩:v1 只对向量做量化,`text`/`meta` 原样存储(文本仍需供 BM25 使用);
  超长期下若文本成为体积大头,压缩列为后续版本能力。

---

## 2. 总体架构

```mermaid
flowchart LR
    subgraph 宿主程序
        APP["Agent 框架<br/>(LangChain / 自研)"]
    end
    subgraph "Mneme 库(单 crate)"
        F["lib.rs 门面<br/>Mneme · Namespace · Builder<br/>feature:async → spawn_blocking"]
        subgraph "L6 quant/"
            QT["i8/f16 量化副本<br/>两阶段重打分"]
        end
        subgraph "L5 life/"
            LIFE["TTL · retain 遗忘曲线<br/>compaction 调度 · 快照备份 · stats"]
        end
        subgraph "L4 query/"
            QUERY["过滤 DSL + 计划器<br/>BM25 + RRF 融合 · 去重预检"]
        end
        subgraph "L3 index/"
            HNSW["HNSW 图<br/>过滤三档搜索"]
        end
        subgraph "L2 persist/"
            STORE["WAL · vsec/msec/hidx 段文件<br/>MANIFEST · 恢复 · trash"]
        end
        subgraph "L1 memory/"
            MEM["内存表 · 暴力扫描<br/>过滤 AST · 公开 API"]
        end
        subgraph "L0 core/"
            CORE["类型/错误/ID<br/>SIMD 距离 · TopK 堆 · varint"]
        end
    end
    DISK[("本地目录<br/>agent_memory/")]

    APP --> F
    F --> QUERY
    QUERY --> HNSW
    LIFE --> QUERY
    HNSW --> STORE
    QT --> HNSW
    LIFE --> STORE
    F --> MEM
    MEM --> CORE
    HNSW --> CORE
    QUERY --> CORE
    STORE --> DISK
```

数据流与"一次查询的全流程"见 [00 §8](00-fundamentals.md)。

### 2.1 并发模型(全局决策)

- **单写者多读者**:写路径全局串行(一个 `Mutex`),读路径通过
  `RwLock<Arc<ReaderView>>` 拿到不可变的"段列表 + 可变表快照"视图后**无锁读**;
- **MVCC**:全局 seqno + 水位实现快照读(原理见 [00 §6.6](00-fundamentals.md));
- **同步核心 + async 门面**:核心库零 tokio 依赖;`feature = "async"` 提供的
  async API 全部是 `spawn_blocking` 薄包装([08 §6](08-l6-quant.md));
- **线程安全**:`Mneme`/`Namespace`/`SnapshotHandle` 均为 `Send + Sync`,可跨线程共享;
  完整保证见 [11 §5](11-api-reference.md)。

---

## 3. 渐进式分层(建造顺序)

每一层只依赖下层;**每层完成时都是一个可交付的完整产品**;层边界 = 稳定接口。

| 层 | 目录 | 内容 | 完成后的可用形态 | 验收(详见 [09](09-testing.md)) |
|---|---|---|---|---|
| L0 | `core/` | 类型/错误/ID、SIMD 距离、TopK 堆、varint | 无 I/O 数学库 | 距离函数对照测试 |
| L1 | `memory/` | 全内存引擎、暴力扫描、过滤 AST、去重预检 | **纯内存向量库**(易失,可用于测试/缓存);**公开 API 在此冻结** | 全 API 集成测试 |
| L2 | `persist/` | WAL、vsec/msec 段、MANIFEST、恢复、墓碑删除 | 重启不丢数据;WAL 超限自动全量快照兜底 | 崩溃注入测试全绿 |
| L3 | `index/` | 自研 HNSW、hidx 持久化、过滤三档搜索 | 同一 API 下暴力→ANN 无感升级;mmap 引入(可关) | Recall@10 ≥ 0.95 |
| L4 | `query/` | 过滤 DSL 解析、zone map 下推、BM25+RRF、去重 | 混合检索可用 | 混合检索集成测试 |
| L5 | `life/` | TTL、遗忘曲线、size-tiered compaction、命名空间、快照/备份、stats | **超长期闭环**:段数有界、自动遗忘 | 24h 长跑测试 |
| L6 | `quant/` | i8/f16 量化+重打分、async 门面完善、基准、fuzz | 达到全部性能目标 | 基准达标 |

**渐进式的两个关键手段**:

1. **接口先于实现**:L1 就定义内部 trait `VectorStore`(暴力与 HNSW 同签名),
   L3 只是替换实现;L2 的段文件头从第一天就带 `format_version` 字段。
2. **每层有兜底**:L2 阶段(还没有 compaction)用"WAL 总量超 256MB 自动全量快照兜底"([04 §3.2](04-l2-persist.md));
   L3 永远保留暴力扫描作为过滤极端选择性时的第三档策略。
   系统在每一层都是"完整能跑"的,性能和功能是逐层叠加的。

---

## 4. 单 crate 模块布局

**决定:单 crate 起步,不做 workspace。** 模块间只允许向下依赖(L 序号大的依赖小的),
`core` 之外禁止横向穿透。若未来某边界确需独立发布,再按现成模块边界拆分(不在承诺内)。

```text
mneme/
├── src/
│   ├── lib.rs          # 门面:Mneme / Namespace / Builder;pub use 公开类型
│   ├── core/           # L0:types.rs error.rs metric.rs simd.rs varint.rs meta.rs heap.rs options.rs
│   ├── memory/         # L1:table.rs engine.rs search.rs pred.rs dedup.rs
│   ├── persist/        # L2:wal.rs vsec.rs msec.rs manifest.rs recover.rs flush.rs source.rs trash.rs
│   ├── index/          # L3:hnsw.rs graph.rs filtered.rs merge.rs rebuild.rs
│   ├── query/          # L4:parse.rs plan.rs zmap.rs bm25.rs fusion.rs dedup.rs exec.rs
│   ├── life/           # L5:ttl.rs retain.rs access.rs namespace.rs compact.rs backup.rs stats.rs
│   └── quant/          # L6:scalar_i8.rs f16.rs rescore.rs
├── benches/            # criterion 基准(L3 起)
├── fuzz/               # cargo-fuzz 目标(L6 起)
├── docs/               # 本文档
└── Cargo.toml
```

---

## 5. 依赖白名单

**原则:std 优先,复杂能力一律自研。** 外部依赖只允许"小、稳、无传递依赖(或传递极少)"的
基础设施件;新增任何依赖必须在 PR 中论证。复杂算法(HNSW、BM25、量化、bloom、
compaction 调度、分词)**全部自研**。

| 依赖 | 用途 | 进入层级 | 隔离措施 |
|---|---|---|---|
| `thiserror` | 错误派生宏 | L0 | 仅宏便利,可随时手写替换 |
| `serde` + `serde_json` | 元数据 Value、DSL 序列化 | L0 | **只允许** `core/meta.rs` 接触,外部只见 `Meta` 别名 |
| `crc32fast` | CRC-32 校验 | L2 | 无传递依赖 |
| `memmap2` | mmap 零拷贝读 | L3 起 | feature `mmap`(默认开);关闭走 `Read+Seek` 兜底 |
| `half` | f16 转换 | L6 | feature `quant-f16` |
| `tokio` | async 门面 | 门面 | feature `async`(默认关);核心零 tokio |

> **默认构建口径**:`thiserror` + `serde` + `serde_json` + `crc32fast` = 4 个强依赖;
> `mmap` 默认开启会额外引入 `memmap2`,故**默认构建实为 5 个**。`memmap2`/`half`/`tokio`
> 都随 feature 走,`--no-default-features` 可回到 4 个。

**明确不自引**(自研替代):`rayon`(用 `std::thread::scope`)、`crossbeam`(用
`std::sync::mpsc`)、`parking_lot`/`arc-swap`(用 std `RwLock`/`Mutex`)、
`zerocopy`(手写编解码)、`xxhash`(crc32 足够)、`unicode-segmentation`
(手写分词:空白切词 + CJK bigram)、任何现成 HNSW/BM25 库。

dev-dependencies(不进入发布产物):`proptest`、`tempfile`、`criterion`。

### 5.1 Feature 总表

| feature | 默认 | 引入 | 说明 |
|---|---|---|---|
| `mmap` | ✅ 开 | `memmap2` | 关闭后段文件走 `FileSource`(`Read + Seek`),功能不变、稍慢 |
| `async` | ❌ 关 | `tokio` | 提供 `insert().await` 等 async API |
| `quant-f16` | ❌ 关 | `half` | 提供 f16 量化副本;关闭时只有 f32/i8 |

---

## 6. 公开 API(在 L1 冻结)

以下类型与方法的**签名在 L1 完成时冻结**,后续层只升级实现。完整语义见
[03 章](03-l1-memory.md),此处为速览。

```rust
// ---- 构建 / 打开 ----
let db = Mneme::builder()
    .path("./agent_memory")              // 省略则 = 纯内存(须用 .dimension(d) 或 in_memory(d))
    .dimension(1536)                     // 新建必填;打开时校验,建库后不可改
    .metric(Metric::Cosine)              // 默认 Cosine
    .fsync(FsyncPolicy::Batched(Duration::from_millis(20)))
    .insert_mode(InsertMode::Upsert)     // 同 key 行为
    .dedup(Dedup::Off)                   // 去重策略
    .build()?;                           // -> Result<Mneme>
// let db = Mneme::open("./agent_memory")?;  // 打开已有库:维度/度量从 MANIFEST 读回

let ns = db.namespace("agent-42/session-88");   // 分层命名空间

// ---- 写入 ----
let outcome = ns.insert(
    Record::new(vector)                          // 维度在 insert 时校验,故无 Result
        .key("mem_001")                          // 外部键,可选;重复即 upsert
        .text("用户偏好深色模式")                  // 可选;启用 BM25 与文本去重
        .metadata(json!({"kind":"preference"}))   // 可选 JSON;importance 用 .importance() 设
        .importance(0.8)                          // 可选;默认 0.5
        .ttl(Duration::from_secs(30 * 86400))    // 可选;到期自动遗忘
)?;   // -> InsertOutcome { Inserted(RowId) | Duplicate{..} }(开了去重时)

let outcomes = ns.insert_batch(batch)?;          // 批量原子写入(50k/s 目标的公开入口)

// ---- 检索(向量 + 过滤) ----
let hits: Vec<Hit> = ns.search().vector(&q)
    .top_k(10).ef(128)
    .filter(filter!(r#"kind == "preference" && importance > 0.5"#))
    .execute()?;

// ---- 混合检索(向量 + 关键词,RRF 融合) ----
let hits = ns.search().vector(&q).text("深色模式")
    .fusion(Fusion::Rrf { k: 60 }).top_k(10).execute()?;

// ---- 单点读 / 删除 / 遍历 ----
let rec: Option<RecordRef> = ns.get("mem_001")?;
if let Some(r) = &rec { let _v = r.vector(); }    // 原始向量(或 get_vector(rowid))
ns.delete("mem_001")?;            // 无 key 记录用 delete_by_rowid(rowid)
for row in ns.iter(Some(filter!("kind == \"scratch\"")))? { let rec = row?; /* ... */ }

// ---- 记忆生命周期 ----
ns.touch("mem_001", None)?;                               // 记忆强化(访问计数+1;Some(d) 可提 importance)
ns.forget(filter!("kind == \"scratch\""))?;               // 主动遗忘
ns.retain(Retention::new()
    .half_life(Duration::from_secs(14 * 86400))           // 遗忘曲线半衰期
    .min_importance(0.2)
    .protect(filter!("kind == \"decision\"")))?;          // 白名单豁免

// ---- 命名空间 / 运维 ----
let names = db.list_namespaces()?;
db.drop_namespace("agent-42/session-88")?;                // 含子命名空间
let snap = db.snapshot();                                 // 钉住当前版本的只读句柄
let s = db.stats()?;        // 段数/行数/WAL 尺寸/延迟直方图/每 NS 统计
db.check()?;                // fsck:校验 CRC 与索引一致性
db.backup_to("./backup")?;  // 一致性快照备份
db.flush()?;                // 显式落盘(把可变表写成段)
db.close()?;                // flush + 释放文件锁;Drop 只尽力 flush
```

完整签名、配置总表、打开校验、错误/重试与备份 runbook 见
[11 公开 API 与运维参考](11-api-reference.md)。

公开类型速览:

| 类型 | 说明 |
|---|---|
| `Record` | 一条记忆:可选 key、向量、可选 text、元数据(JSON)、可选 TTL、可选 importance |
| `Hit` | 检索命中:RowId(全局稳定)、key、score、记录视图(不含向量) |
| `RecordRef` | 存储记录只读视图(无 score),用于 `get`/`iter`;`vector()` 取回原始向量 |
| `InsertOutcome` | `Inserted(RowId)` 或 `Duplicate { existing: RowId, score }`(开启去重时) |
| `Metric` | `Cosine / Dot / Euclidean` |
| `FsyncPolicy` | `Always / Batched(Duration) / OnFlush / Never` |
| `InsertMode` / `Dedup` / `ResultDedup` | 同 key 行为 / 写入期去重 / 结果级去重 |
| `Expr` / `filter!` | 过滤 AST 与字符串解析宏 |
| `Fusion` | `Rrf { k } / Weighted { alpha }` |
| `Retention` | 遗忘策略:半衰期、重要性下限、保护过滤器 |
| `VectorFormat` | `F32 / F16 / I8Rescored`(L6 量化) |
| `HnswParams` / `CompactionPolicy` / `Limits` | 索引 / 合并 / 数据限额配置 |
| `Clock` | 时间源注入(测试确定性) |
| `SnapshotHandle` | 钉住某 ReaderView(段集 + 可变表快照)的只读句柄(时间旅行读) |
| `Stats` / `CheckReport` / `BackupReport` | 运维报告 |
| `MnemeError` | 统一错误(见 [02 §2](02-l0-core.md)) |

---

## 7. 与同类方案的一句话对比

| 方案 | 形态 | 相对 Mneme 的差异 |
|---|---|---|
| Qdrant / Milvus | 服务端向量库 | 需独立部署与运维;过滤/量化思路可借鉴,Mneme 做成进程内 |
| LanceDB | 嵌入式,列式格式 | 通用数据湖导向;Mneme 更小而专:自研格式 + Agent 生命周期特性 |
| sqlite-vec | SQLite 扩展 | 借 SQLite 生态,暴力为主;Mneme 自带 HNSW 与记忆生命周期 |
| redb/fjall + 自拼索引 | 自组装 | 没有记忆语义(TTL/衰减/去重)与统一混合检索,需大量胶水层 |
| mem0 / Letta | 记忆编排框架 | 它们是 Mneme 的**典型客户**:编排层决定"记什么、忘什么",Mneme 提供引擎级支撑 |

---

## 下一章

[02-l0-core.md](02-l0-core.md):从最底层开始——距离度量的数学、SIMD、TopK 堆与 varint。
