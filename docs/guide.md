# Mneme 开发者指南

> **这份文档给谁看**:准备在自己的程序里使用 Mneme 的开发者——从第一次 `cargo add`
> 到把它开进生产。它按"任务"组织:建模、写入、检索、生命周期、持久化、调优、排障。
>
> **与其它文档的分工**:
>
> - 想弄懂"为什么这样设计"与完整数学推导 → [00 零基础篇](design/00-fundamentals.md) 与 [02–12 各层设计](design/02-l0-core.md);
> - 想照抄典型记忆模式(会话记忆/长期偏好/沉淀/遗忘) → [13 记忆模式手册](design/13-cookbook.md);
> - 需要精确签名、配置总表、备份 runbook、数据限额 → [16 公开 API 与运维参考](design/16-api-reference.md);
> - 形式化契约与不变量定义(测试与追溯的锚点) → [spec/contracts.md](spec/contracts.md)。

---

## 1. 先判断:什么时候该用 Mneme

**适合**:

- 你有一个进程内的 Agent / 应用,需要**跨会话、跨天**记住事实、偏好、事件与它们之间的关系;
- 你想把"记忆"做成**本地一个目录**,无服务、无网络、零运维,像 SQLite 一样随程序走;
- 你需要**引擎级的记忆语义**:命名空间隔离、TTL/遗忘曲线、去重合并、混合检索、时间旅行;
- 你关心**超长期**:数据逐年增长,段数有界、内存有上界、格式可演化。

**不适合**:

- 多进程并发**写**(单写者;多进程只读共享是支持的,见 §6.2);
- 分布式、副本、分片、多租户鉴权(安全边界是宿主进程的文件系统权限);
- 内置 embedding 推理(Mneme 只存向量,向量由你调用外部模型产生);
- 通用 SQL / 复杂查询语言(只有记忆检索所需的过滤 DSL)。

一句话对比见 [01 §7](design/01-overview.md);与 mem0/Letta 这类编排框架的关系是
**互补**:编排层决定"记什么、忘什么",Mneme 提供引擎级支撑([16 §6](design/16-api-reference.md))。

---

## 2. 五分钟上手

### 2.1 引入依赖与 feature 选择

```toml
[dependencies]
# 默认构建:含 mmap 段读取(推荐)
mneme-db = "0.1"

# 常用组合
# mneme-db = { version = "0.1", features = ["async"] }          # async 门面(引入 tokio 的 rt)
# mneme-db = { version = "0.1", features = ["quant-f16"] }      # f16 量化副本(引入 half)
# mneme-db = { version = "0.1", features = ["encrypt", "compress"] }  # 静态加密 + 压缩
# mneme-db = { version = "0.1", default-features = false }      # 关闭 mmap(纯 Read+Seek 兜底)
```

> `mneme` 已被 crates.io 占用,发布名为 `mneme-db`;库名仍为 `mneme`,
> 代码里照旧 `use mneme::...`。八个 feature 的作用与默认值
> 见 [README 的 feature 表](../README.md)与 [16 §2](design/16-api-reference.md)。

### 2.2 最小可用程序

完整流程:**建库 → 写入 → 检索 → 反馈 → 关闭**。

```rust
use mneme::{filter, json, Diversity, Feedback, FsyncPolicy, Metric, Mneme, Record, Scoring};
use std::time::Duration;

fn main() -> mneme::Result<()> {
    // 1. 打开(或新建)一个本地目录作为记忆库
    let db = Mneme::builder()
        .path("./agent_memory")                 // 省略 path = 纯内存库(易失,需 .dimension)
        .dimension(1536)                        // 新建必填;打开已有库时以 MANIFEST 为准
        .metric(Metric::Cosine)                 // 默认 Cosine,建库后不可改
        .fsync(FsyncPolicy::Batched(Duration::from_millis(20)))
        .build()?;
    let ns = db.namespace("agent-42/profile"); // 分层命名空间,不存在则首次写入时登记

    // 2. 记住两条信息(向量由宿主调用嵌入模型产生)
    ns.insert(
        Record::new(embed("用户喜欢深色模式")?)    // embed(...) 是宿主侧函数,不是库 API
            .key("pref.theme")                     // 可选外部键;重复即按 Upsert 处理
            .text("用户喜欢深色模式")               // 可选;启用 BM25 与文本去重
            .metadata(json!({"kind": "preference"}))
            .importance(0.8),                      // 默认 0.5,参与记忆感知排序
    )?;
    ns.insert(
        Record::new(embed("用户用 macOS")?)
            .key("pref.os")
            .text("用户用 macOS")
            .metadata(json!({"kind": "fact"}))
            .importance(0.6),
    )?;

    // 3. 混合检索:向量 + BM25 + 过滤 + 综合打分 + MMR 多样性
    let q = embed("他喜欢什么界面风格?")?;
    let hits = ns
        .search()
        .vector(&q)
        .text("界面风格")                          // 双通道;融合默认 RRF(k=60)
        .filter(filter!(r#"kind == "preference""#)) // 写死的字面量用 filter!;运行时输入用 Expr::from_str
        .score(Scoring { w_recency: 0.2, w_importance: 0.3, ..Scoring::default() })
        .diversify(Diversity::Mmr { lambda: 0.7 })
        .top_k(10)
        .execute()?;
    for h in &hits {
        println!("{:?} {:?} score={:.3}", h.rowid, h.key, h.score);
    }

    // 4. 反馈闭环:告诉引擎哪些结果真的被用上了((rowid, query_id) 幂等)
    if let Some(h) = hits.first() {
        ns.feedback(h.rowid, Feedback::Used, h.query_id)?;
    }

    // 5. 优雅关闭:返回 Ok 即所有已确认写入已持久
    db.close()?;
    Ok(())
}
```

> **想直接跑真实嵌入 API**:`examples/memory` 是可直接运行的交互式版本(REPL:
> `/add`、`/search`、`/get`、`/list`、`/touch`、`/delete`),配置与跑法见
> [README「端到端示例」](../README.md);其离线路径用例随 `cargo test` 一起跑。

### 2.3 从示例里要记住的五件事

1. **维度是建库属性**:新建时必填,之后由 MANIFEST 持久化,打开时无需再传;
   显式传错会拒绝打开(`DimensionMismatch`),绝不静默改写([16 §3](design/16-api-reference.md))。
2. **`insert` 无 `Result` 的构造阶段**:维度校验发生在 `insert` 时,所以 `Record::new` 不返回 `Result`。
3. **`filter!` 只给写死的字面量用**:运行时输入请用 `Expr::from_str` 拿 `Result`(不 panic);
   `filter!` 对非法字面量 panic,是文档化例外。
4. **`close()` 才保证持久**:`Drop` 不 flush、不保证;进程崩溃时已确认写入靠 WAL 恢复,
   未确认的写入可能丢([16 §1.7](design/16-api-reference.md))。
5. **`Hit` 不含向量**(控制内存):要向量用 `get_vector(rowid)` 或 `get`。

---

## 3. 心智模型:一条记忆的一生

理解下面这条链路,后面所有配置与取舍都会变得直观:

```text
写入                    检索                      持久化与整理
─────────────           ─────────────            ─────────────────
Record                  query vector/text         WAL(预写日志)
  │                       │                        │ fsync 后即"已确认"
  ▼                       ▼                        ▼
校验(维度/限额/有限值)   命名空间隔离              可变表(内存)
  │                       │                        │ flush(手动/自动)
  ▼                       ▼                        ▼
写入事务(全局串行)     过滤计划(zone map/bloom)    不可变段 + MANIFEST 原子提交
  │  分配 RowId/seqno      │                        │ compaction(size-tiered)
  ▼                       ▼                        ▼
版本链最新版本          候选 → ANN/暴力 → 打分      段数有界、历史按窗口回收
                         │
                         ▼
                     Hit(带 query_id)
```

**八个术语**([15 术语表](design/15-glossary.md) 有完整定义):

| 术语 | 一句话 |
|---|---|
| **命名空间(Namespace)** | 记忆的逻辑分区,键唯一性按命名空间隔离;路径式 `agent-42/session-88` |
| **RowId** | 记录的全局稳定标识;`update`/upsert 不改变它,关系边与访问统计都挂在它上面 |
| **版本链** | 同一 RowId 的历次写入版本;`as_of(t)` 沿链取历史,compaction 按窗口回收 |
| **段(Segment)** | 不可变的磁盘文件三元组 `(vsec, msec[, hidx])`;只追加、不修改 |
| **WAL** | 预写日志;已 fsync 的帧即提交点,崩溃后据此恢复未落段的写入 |
| **MANIFEST** | 活跃段集合与各种水位的原子提交点;当前库由 `current` 指向某版本 |
| **快照(Snapshot)** | 钉住某个不可变读视图的句柄;存活期内看到固定的一致过去 |
| **compaction** | 把多个段合并重写、回收墓碑/过期/超期历史,保持段数有界 |

**并发模型**:单写者多读者。写路径全局串行(一把写锁);读路径拿到不可变视图后无锁扫描,
同一快照内任意并发查询结果完全一致。`Mneme`/`Namespace`/`SnapshotHandle` 都是
`Send + Sync`,克隆廉价([16 §5](design/16-api-reference.md))。

---

## 4. 集成全流程

### 4.1 打开与建库

```rust
// 新建(目录不存在):dimension 必填
let db = Mneme::builder().path("./mem").dimension(1536).build()?;

// 打开已有库:维度/度量从 MANIFEST 读回;也可显式声明,不一致会拒绝打开
let db = Mneme::open("./mem")?;
// 等价于 Mneme::builder().path("./mem").build()?(不传 dimension)

// 纯内存库(测试/缓存;易失)
let db = Mneme::builder().dimension(1536).build()?;
```

要点:

- 一个目录 = 一个库。写实例对目录持 **OS 文件锁**,第二个写实例得到 `Busy`,
  进程退出/崩溃后锁由内核自动释放([16 §3](design/16-api-reference.md));
- 维度、度量不可变;fsync、去重、量化、HNSW、compaction、limits 等**每次打开都可调整**;
- 打开时默认不做全量 CRC 校验(快);需要"启动即验"用 `.verify_on_open(true)`,
  需要"损坏即拒启"用 `.fail_fast_on_corruption(true)`(默认改为隔离跳过该段,[04 §7](design/04-l2-persist.md))。

### 4.2 命名空间布局

按**生命周期**划分,而不是按数据类型:

```text
agent-42/                    # 一个 Agent
├── profile/                 # 长期偏好与事实(高 importance,无 TTL)
├── session-88/              # 会话记忆(短 TTL,随会话结束清理)
└── knowledge/               # 沉淀后的语义记忆(consolidate 产生)
```

```rust
let profile = db.namespace("agent-42/profile");
let session = db.namespace("agent-42/session-88");
let names   = db.list_namespaces()?;        // 规范化路径字典序
let removed = db.drop_namespace("agent-42/session-88")?; // 级联墓碑含子空间,返回行数
```

为什么按生命周期分:TTL、retention、compaction 的策略都按命名空间/过滤器作用域设计,
把"会过期的"与"永久的"混在一个空间里,任何遗忘策略都会误伤([13 §1](design/13-cookbook.md))。

### 4.3 写入

```rust
// 单条
let outcome = ns.insert(Record::new(v).key("mem_001").text("…").ttl(Duration::from_secs(30 * 86400)))?;

// 批量:整批原子,共用一次 fsync;任一条校验失败整批拒绝(零部分写入)
let outcomes = ns.insert_batch(vec![rec_a, rec_b, rec_c])?;

// 局部更新:保留 RowId,只改给定字段
ns.update("mem_001", UpdatePatch::new().importance(0.9).text(Some("新文本".into())))?;
ns.update_by_rowid(rowid, UpdatePatch::new().ttl(None))?;  // None = 取消过期(text/metadata 的 None = 清空)

// 删除(返回是否命中活记录)
ns.delete("mem_001")?;
ns.delete_by_rowid(rowid)?;
```

**写入语义速记**:

- `InsertMode::Upsert`(默认):同 key 保留既有 RowId、写入新版本;`RejectDuplicate`:同 key 已存在可见记录时返回 `DuplicateKey`;
- **去重四模式**(`.dedup(..)`,建库时确定):`Off` 不去重;`Reject` 返回 `Duplicate` 拒绝;
  `Replace` 墓碑旧行、新 RowId;`KeepBoth` 照常写入;`Merge(回调)` 就地更新并保留旧 RowId;
  阈值用 `.dedup_threshold(0.95)`,统一按余弦口径(非余弦度量内部先归一化);
- **限额默认值**:key ≤ 1 KiB、text ≤ 1 MiB、metadata ≤ 64 KiB/32 层;超限返回
  `TooLarge`/`MetaTooDeep`,绝不截断([16 §8](design/16-api-reference.md));
- **有限性校验**:向量分量与 `importance`/`confidence`/边权含 `NaN`/`±Inf` → `NonFinite`,绝不入库。

`InsertOutcome` 的三种结果(`Inserted`/`Merged`/`Duplicate`)语义见
[16 §1.2](design/16-api-reference.md);`update` 返回 `Updated(RowId)` 或 `NotFound`。

### 4.4 检索

```rust
let hits = ns.search()
    .vector(&q)                                   // 向量通道
    .text("深色模式")                              // BM25 通道;与向量通道融合(默认 RRF)
    .filter(Expr::from_str(r#"kind == "preference" && importance > 0.5"#)?)  // 预过滤
    .ef(128)                                      // HNSW 探查宽度;上限 4096
    .score(Scoring { w_recency: 0.2, w_importance: 0.3, ..Default::default() })
    .diversify(Diversity::Mmr { lambda: 0.7 })    // 结果多样性
    .dedup(ResultDedup::Near { threshold: 0.95 }) // 结果级去重
    .expand(RelationExpand { hops: 1, kinds: vec![RelationKind::SUPPORTS], decay: 0.5, max_nodes: 64 })
    .top_k(10)
    .execute()?;
```

**查询通道与融合**:

- 至少启用一个通道,否则 `execute()` 返回 `Config`;
- `fusion(Fusion::Rrf { k: 60 })`(默认)或 `Fusion::Weighted { alpha: 0.5 }`;
  只设一个通道又显式设 `fusion` 会返回 `Config`(设置即拒绝,不静默忽略);
- 未开 `Scoring` 时 `Hit.score` 是原始相似度/距离(按 `Metric::better` 方向最优在前);
  开启后为综合分(越大越优),同分按 RowId 升序——同一快照内**排序全等**。

**过滤 DSL**([06 §1](design/06-l4-query.md) 全文法):

```text
kind == "preference" && importance > 0.5
created_at > now - 7d
tag in ("work", "home")
text contains "深色"        // 数组含元素或字符串含子串
name ~ "mem_*"              // 通配符
exists(agent_id)            // 字段存在(可为 null);查"缺失"用它
```

- 保留字段(`importance`/`created_at`/`rowid`/`expires_at`/`access_count`…)优先于同名 metadata,
  完整清单见 [16 §8.1](design/16-api-reference.md);
- **三值语义**:字段缺失时比较条件为"不命中",且 `not(kind == "x")` **不会**命中无 `kind` 的记录;
- 运行时输入用 `Expr::from_str`,写死字面量用 `filter!`;
  `Expr` 可经 `to_meta`/`from_meta` 做 JSON 往返,便于框架下发([06 §1](design/06-l4-query.md))。

**点读与遍历**(不参与 ANN,适合导出/审计):

```rust
let one = ns.get("mem_001")?;                       // Option<RecordRef<'_>>
let v = ns.get_vector(rowid)?.unwrap_or_default();  // 单独取向量,不物化整条
let n = ns.count(Some(filter!(r#"kind == "scratch""#)))?;
for row in ns.iter(Some(filter!(r#"kind == "scratch""#)))? {
    let rec = row?;                                 // 逐行 Result:I/O 失败不会被静默截断
}
```

### 4.5 记忆模型:关系、双时态、沉淀、反馈

```rust
// —— 关系图:记忆之间显式建边,检索时可联想扩展 ——
ns.relate(a, b, RelationKind::SUPPORTS, 0.8)?;
ns.relate_with_options(a, b, RelateOptions::new(RelationKind::SUPPORTS, 0.8).metadata(json!({"why": "user"})))?;
let out_edges = ns.neighbors(a, &[RelationKind::SUPPORTS])?;      // 出边
let in_edges  = ns.predecessors(b, &[RelationKind::SUPPORTS])?;   // 入边(想快开 RelationIndex::Both)
let custom = ns.relation_kind("my_kind")?;                        // 注册自定义类型(同名幂等,重启后一致)

// —— 双时态:事务时间(什么时候写入)+ 有效时间(事实何时为真) ——
ns.supersede("pref.theme", Record::new(v2).valid_from(ts_ms))?;   // 信念修订:旧版本 valid_to 闭合
let old = db.as_of(ts("2024-03-01"))?;                            // 历史快照句柄(可反复读)
let old_hits = ns.search().vector(&q).as_of(ts("2024-03-01")).execute()?; // 或单次历史检索
// (`ts(...)` 是宿主侧的"ISO 8601 → Unix 毫秒"辅助函数;库内时间统一为 i64 毫秒)

// —— 记忆沉淀:把一批 episodic 聚成 semantic 摘要 ——
let report = ns.consolidate(ConsolidationPolicy::default())?;     // clusters/merged/created

// —— 反馈闭环:结果被用上就回写为记忆强化 ——
ns.feedback(hits[0].rowid, Feedback::Used, hits[0].query_id)?;    // (rowid, query_id) 幂等
```

关系边不可见规则:任一端被删除/过期,该边在读取时不可见(悬挂边不返回,[09 §2](design/09-memory-model.md))。
双时态默认**永久保留**历史版本;要约束磁盘占用,设 `CompactionPolicy.history_horizon`
(如 90 天),代价是更早历史不可回溯([07 §4.2a](design/07-l5-life.md))。

### 4.6 生命周期:TTL、访问强化与遗忘

```rust
// TTL:写入时给出相对时长,引擎换算成绝对 expires_at;到期逻辑过期、物理回收留给 compaction
ns.insert(Record::new(v).key("scratch").ttl(Duration::from_secs(7 * 86400)))?;

// 访问强化:读路径命中会攒批统计;手动 touch 可显式强化(boost 提升 importance)
ns.touch("mem_001", None)?;
ns.touch("mem_001", Some(0.1))?;

// 主动遗忘:按过滤器打墓碑(物理回收仍由 compaction 完成)
ns.forget(filter!(r#"kind == "scratch""#))?;

// 遗忘曲线:半衰期 + 重要性下限 + 保护白名单
ns.retain(Retention::new()
    .half_life(Duration::from_secs(14 * 86400))
    .min_importance(0.2)
    .protect(filter!(r#"kind == "decision""#)))?;
```

**安全默认**:自动遗忘**默认关闭**,只有显式 `.retention(Some(policy))` 才在后台运行;
`RetainReport` 给出 `forgotten` 与抽样 `sampled_ids`,删除可审计
(墓碑默认永久保留,经 `iter_with(.., true)` 可见)——[07 §3.4](design/07-l5-life.md)。

### 4.7 持久化、关闭与备份

**fsync 策略怎么选**:

| 策略 | 语义 | 适用 |
|---|---|---|
| `Batched(20ms)`(默认) | 写线程每 d 毫秒统一 fsync;提交者等自己的水位落盘 | 绝大多数生产场景 |
| `Always` | 每次事务立即 fsync 后返回 | 不能丢任何一条已确认写入 |
| `OnFlush` | 不在提交时等待,由 flush 统一 fsync | 批量导入、可接受崩溃丢尾部 |
| `Never` | 只写页缓存 | 仅测试 |

**关键语义**([04 §3](design/04-l2-persist.md)):

- WAL 帧成功 fsync 即提交点;之后后台 flush 失败不回滚已提交写;
- 崩溃恢复:已确认写入完整可见,删除永不复活;撕裂尾部被物理截断,不遮挡后续写入;
- `close()` 返回 `Ok` 后所有已确认写入持久;**`Drop` 不保证**。

```rust
db.flush()?;                 // 把可变表落成增量段并 fsync WAL(显式整理)
db.compact()?;               // 显式触发一轮 size-tiered compaction(预算内合并段组)
db.maintenance_tick()?;      // 手动执行一轮后台维护(访问攒批/自动遗忘/自动 compaction)
let report = db.backup_to("./backup")?;  // 一致性备份:先 flush 再复制/硬链接
db.close()?;                 // flush + 停后台维护 + 释放锁;幂等
```

**批量导入期**:用 `Builder::maintenance(false)` 闸住后台 compaction/遗忘,避免与导入争抢
CPU/IO;建完统一 `compact()` 整理([07 §4](design/07-l5-life.md))。

---

## 5. 配置与调优

### 5.1 先默认跑通,再按指标调

所有旋钮都有默认值。调优顺序建议:

1. 看 `db.stats()`:段数、WAL 尺寸、每命名空间行数、查询延迟直方图、量化状态、compaction 状态;
2. 读路径慢 → 先开量化(`I8Rescored`)、再调 `ef`、考虑 `Scoring` 的服务端排序成本;
3. 写路径慢 → 检查 fsync 策略与批量写入(用 `insert_batch` 而非逐条);
4. 段数/空间增长 → 检查 `history_horizon` 与 retention 策略;必要时触发 `compact()`;
5. 打开慢 → `stats().segments` 看段数;段多说明 compaction 没跟上。

### 5.2 场景预设(来自 [16 §2.1](design/16-api-reference.md))

| 场景 | 建议 |
|---|---|
| 会话级临时记忆 | 短 TTL + 默认 `Batched(20ms)`;命名空间按会话分 |
| 长期偏好/事实 | `importance` 显式设高 + `Retention::min_importance` 提高 + `Dedup::Replace`/`Merge` |
| 延迟敏感 | `.quantization(I8Rescored)` + `ef=64~128`;`.parallelism(0)` 交给运行时 |
| 建库吞吐优先 | 默认 `Hybrid` 建图;需逐位复现 f32 图时 `.build_precision(BuildPrecision::F32)` |
| 内存受限 | 默认 mmap + i8;定期 `backup_to` 后重建更小的段 |
| 只读分析副本 | `.read_only(true)` 多进程共享(见 §6.2) |

### 5.3 量化怎么选

- `F32`(默认):不建副本,精度基准;
- `I8Rescored`:每段每维 i8 码流,查询带宽 ÷4;**精度损失由两阶段精排兜底**
  (粗排候选 4k → f32 重排),建段抽样不达标自动回退 f32;
- `F16`(需 feature `quant-f16`):无需参数表、误差无偏,带宽 ÷2,是 i8 的保守替代;
- 注意:**量化省的是查询带宽,不是磁盘**(f32 原向量始终保留供精排,[08 §1](design/08-l6-quant.md));
- 每段实际生效格式看 `stats().quant.active`;`recall_est` 是建段抽样一致率(重开后为 `None`)。

### 5.4 进阶旋钮

- `HnswParams`:`m`(上层度)/`m0`(第 0 层度)/`ef_construction`/`ef_search`;
  默认 16/32/200/64,建库校验域,越界拒绝;
- `CompactionPolicy`:`tier_ratio`/`tier_count`/`dead_ratio`/`wal_bytes`/`wal_file_bytes`/
  `segment_rows`/`io_budget`/`history_horizon`;
  `io_budget` 是**单轮 compaction 的输入字节预算**(默认 0.30 = 一轮最多重写活跃段总字节的 30%),
  它只削节奏、不改变收敛性;
- `Tuning`:暴力分块、字段字典上限、bloom 误判率、过滤三档阈值、两阶段候选倍率、
  建图批参数、flush 切块行数/并行度([16 §2](design/16-api-reference.md));
- `Limits`:key/text/meta 尺寸、深度、`top_k`/`ef` 上限、WAL 单帧上限——调大以内存与
  恢复时间为代价。

flush 切块与块级并行度经 `Tuning.flush_chunk_rows` / `Tuning.flush_threads` 配置
(仅在批量导入调优时使用);库本体绝不读取环境变量,一切调参经 `Builder::*` 显式注入
(heavy 测试档可读同名环境变量后注入 `Tuning`,见 `tests/common/env.rs` 变量清单)。

---

## 6. 上生产

### 6.1 线程与共享

`Mneme`/`Namespace`/`SnapshotHandle`/`SnapshotNamespace` 均 `Send + Sync`;克隆廉价,
可放进 `Arc` 跨线程共享。写路径串行、读路径无锁,不需要你在外面再包一层大锁。
`SearchBuilder` 短生命周期,单线程用完即 `execute`([16 §5](design/16-api-reference.md))。

### 6.2 多进程只读

```rust
// 读进程:不争抢写锁,周期探测新 MANIFEST 并原子切换视图
let db = Mneme::builder()
    .path("./mem")
    .read_only(true)
    .read_only_probe_interval(Duration::from_secs(1))  // 0 = 关闭显式探测
    .build()?;
// 也可手动探测:
let _version = db.reload()?;   // 有新版本 → 切换并返回版本号;无 → None
```

只读实例不创建/不持锁、不写盘;看到的始终是某个已提交 MANIFEST 的完整视图(I29)。
写实例与任意多个只读实例可以并存([12 §2](design/12-deployment.md))。

### 6.3 静态加密与压缩

```rust
use std::sync::Arc;
use mneme::{Cipher, CryptoKey, Encryption, KeyId, Keyring};

// 加密:自描述 AEAD 信封(段/WAL/MANIFEST 全覆盖),需要宿主提供 KeyProvider
// (Keyring 是内置的内存密钥环,生产可换成环境变量/keychain/KMS 实现)
let keyring = Arc::new(Keyring::new(KeyId(1), CryptoKey::generate()?));  // 需 feature encrypt
let db = Mneme::builder()
    .path("./mem")
    .dimension(1536)
    .encryption(Some(Encryption { provider: keyring.clone(), cipher: Cipher::Aes256Gcm }))
    .build()?;
let new_key = db.rotate_encryption_key()?;   // 轮换:全量段重写;完成后可 Keyring::retire 旧密钥

// 压缩:记录体 text/meta/provenance 按字段压缩,无收益自动回退原文
let db = Mneme::builder().path("./mem").dimension(1536)
    .compression(Compression::Lz4)            // 需 feature compress;Zstd 需 compress-zstd
    .build()?;
```

加密段放弃 mmap(解密进自有缓冲);未开对应 feature 却设置配置 → `Unsupported`,
绝不静默明文落盘([11](design/11-security-storage.md))。

### 6.4 WASM 与自定义后端

```rust
// 宿主自定义存储后端(内存文件系统/IndexedDB 等)
let db = Mneme::builder()
    .path("./mem")                           // 逻辑根路径;由后端解释(FS 后端即真实目录)
    .dimension(1536)
    .storage(Arc::new(MyStorage::new()))     // 需实现 Storage trait(见 12 §3)
    .build()?;
```

WASM 场景用 feature `wasm`(关闭 mmap 与后台线程)配合 `MemStorage`/宿主后端;
目标构建由 CI `wasm-check` 验证([12 §3](design/12-deployment.md))。

### 6.5 可观测

引擎不引 `log`/`tracing`。两种方式看运行状况:

```rust
// 轮询:stats() + check()
let s = db.stats()?;            // 段/WAL/延时直方图/每 NS/量化/compaction/历史
let c = db.check()?;            // fsck:CRC + 索引对账 + 合并建议

// 事件流:注册 Observer 接 Query/Write/Flush/Compaction/Error(回调 panic 被隔离)
let db = Mneme::builder().path("./mem").dimension(1536)
    .observer(Arc::new(MyMetrics))
    .build()?;
```

建议把 `stats()` 定期采样上报(段数趋势、P99 延迟、WAL 尺寸),把 `Observer` 桥接到你
现有的指标系统([12 §4](design/12-deployment.md))。

### 6.6 容量估算(1M × 1536 维,[16 §11](design/16-api-reference.md))

| 项 | 量级 |
|---|---|
| f32 向量(始终保留) | ≈ 6.1 GB |
| i8 量化副本(额外) | ≈ 1.5 GB |
| i8 模式段总存储 | ≈ 7.7 GB |
| HNSW 图(hidx) | ≈ 148 MB |

---

## 7. 错误处理与故障排查

### 7.1 错误分类速查

| 错误 | 可重试 | 处置 |
|---|---|---|
| `Io` | ✅ 通常可 | 指数退避;先排查磁盘/权限/ENOSPC |
| `Busy` | ✅ 可 | 另一写实例持锁或正在备份;退避重试或确保单写者 |
| `DuplicateKey` | ✅ 可 | 改 `Upsert`,或先 `get` 再决定 |
| `DimensionMismatch`/`MetricMismatch`/`KeyMismatch` | ❌ | 调用方 bug,修正参数 |
| `FilterParse` | ❌ | DSL 语法错误(带位置),修正表达式 |
| `TooLarge`/`LimitExceeded`/`MetaTooDeep`/`NonFinite` | ❌ | 输入超限,修正数据或调整 `Limits` |
| `Config`/`Unsupported` | ❌ | 配置或能力门控不匹配,按信息调整 |
| `Closed` | ❌ | 库已关闭,不要再使用任何克隆句柄 |
| `UnsupportedVersion`/`Corrupted` | ❌ | 停止写入,跑 `db.check()`,按 §7.3 处置 |
| `Inconsistent`/`IdExhausted` | ❌ | 前者是引擎 bug 请上报;后者是表示空间耗尽(理论不可达) |

完整错误表与触发条件见 [16 §4](design/16-api-reference.md) 与
[spec/contracts.md §0.2](spec/contracts.md)。

### 7.2 重试模板

```rust
fn with_retry<T>(mut f: impl FnMut() -> mneme::Result<T>) -> mneme::Result<T> {
    let mut delay = Duration::from_millis(20);
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(mneme::MnemeError::Busy(_)) | Err(mneme::MnemeError::Io(_)) if delay < Duration::from_secs(2) => {
                std::thread::sleep(delay);
                delay *= 2;                       // 指数退避,上限后放弃
            }
            Err(e) => return Err(e),
        }
    }
}
```

其余错误不要重试:重试 `DimensionMismatch`/`Config` 只会重复失败。

### 7.3 故障处置

**磁盘满(ENOSPC)**:写入返回 `Io`;compaction 暂停而非损坏数据。清理 `trash/` 或扩容后
重试 `flush()`;**不要**手动删除 `wal/` 或 `segments/` 下的文件。

**怀疑数据损坏**:

```text
1. 停止写入(已打开的实例先 close)
2. db.check() 定位损坏段(报告段号/CRC/对账差异)
3. 个别段损坏:默认模式下该段被隔离跳过(文件原地保留),其余数据仍可用;
   用 iter 逐行导出仍完好的数据到新库(逐行 Result 可定位到具体段)
4. MANIFEST 损坏:打开时自动扫描目录回退到上一合法版本(04 §6/§7)
5. 恢复后立即 backup_to 留档,并核对 stats() 行数与 check()
```

更多恢复细节见 [16 §7](design/16-api-reference.md) 的 runbook。

### 7.4 十个常见坑

1. **换嵌入模型 = 新库**:同一库的向量必须来自同一模型且维度一致,不承诺跨模型混用;
2. **`filter!` 用在运行时输入上**:会 panic;运行时用 `Expr::from_str`;
3. **忘记 `close()`**:`Drop` 不保证持久;已确认写入靠 WAL 能恢复,但请显式关闭;
4. **开量化期待磁盘变小**:f32 原向量始终保留,省的是查询带宽;
5. **`metadata` 里塞 `importance`**:保留字段优先,以 `Record::importance()` 为准;
6. **多进程同时写**:第二个写实例 `Busy`;多进程只读请用 `.read_only(true)`;
7. **`Dedup::Merge` 回调想捕获状态**:回调是函数指针;需要状态请编码进记录或用非捕获函数;
8. **期待 compaction 后可复现的 ANN 结果**:重建会重排图,近邻结果可能变化;
   快照只保证"钉住的视图内不变"([05 §6.2](design/05-l3-hnsw.md));
9. **在 `async` 里直接调阻塞方法**:用 `AsyncNamespace`(feature `async`)或自行
   `spawn_blocking`;`search().execute()` 也是阻塞调用;
10. **手动删除 `trash/`/`segments/`/`wal/` 里的文件**:引擎不感知你的删除;
    清理 `trash/` 是安全的(它只是待回收垃圾),其余目录不要碰。

---

## 8. 升级与兼容

- 磁盘格式**只有当前一个版本**:段/MANIFEST/WAL/hidx 的版本号**精确匹配**,
  任何不一致都拒绝打开(`UnsupportedVersion`),绝不静默误读,也不存在
  读取旧开发格式的分支([04 §12](design/04-l2-persist.md));
- 破坏性变更走版本化流程并在 [spec/contracts.md](spec/contracts.md)
  的「变更记录」登记;升级 Mneme 遇格式不匹配时请**从备份恢复或重新灌入**;
- 公开 API 自 L1 冻结;改签名需单独 RFC 并同步 [16](design/16-api-reference.md)。

---

## 9. 上线前检查清单

- [ ] 维度/度量与嵌入模型一致,且已在测试库上验证召回;
- [ ] fsync 策略符合你的丢失容忍度(`Batched` 起步;关键写入用 `Always`);
- [ ] 已显式 `close()`,或确认进程退出路径会调用;
- [ ] 备份任务已接线(`backup_to`),并演练过一次恢复;
- [ ] 自动遗忘若开启,`protect` 白名单与 `min_importance` 已评审(默认关闭);
- [ ] `history_horizon` 已按磁盘预算决策(默认永久保留);
- [ ] 量化开启时确认 `stats().quant.active` 与召回损失可接受;
- [ ] 多进程场景:写进程唯一、读进程 `.read_only(true)`;
- [ ] 指标采集:定期 `stats()` + 可选 `Observer` 桥接;
- [ ] 异常路径:`Busy`/`Io` 有退避重试,`Corrupted` 有 runbook。

---

## 下一步

- 需要具体记忆配方(会话记忆、信念修订、沉淀、安全遗忘、反馈闭环)→ [13 记忆模式手册](design/13-cookbook.md);
- 需要精确签名、配置表、备份 runbook、数据限额 → [16 公开 API 与运维参考](design/16-api-reference.md);
- 想理解引擎内部(持久化格式、HNSW、BM25、量化、compaction)→ [04](design/04-l2-persist.md)–[08](design/08-l6-quant.md) 各层设计;
- 想参与开发 → [docs/DESIGN.md](DESIGN.md) 的"贡献者"路线与 [CONTRIBUTING.md](../CONTRIBUTING.md)。
