# 03 L1 内存引擎:公开 API 在此冻结

> **本章目标**:在全内存中实现**完整的公开 API**——还没有持久化、还没有索引,
> 但一个 Agent 需要的"记住/想起/遗忘"已经全部可用。
> **前置阅读**:[01 §6](01-overview.md)(API 速览)、[02](02-l0-core.md)。
> **本章你将学到**:API 语义细则 → 内存表结构 → 暴力扫描 → 过滤 AST → 去重预检。

模块:`memory/{table.rs, engine.rs, search.rs, pred.rs, dedup.rs}`——`engine.rs` 承载
公开类型 `Mneme` / `Namespace` / `SearchBuilder` 的内存实现,本章 §2 的语义即其行为规约;
其余小节逐个展开数据结构与算法。

---

## 1. 为什么先做内存层(渐进式理由)

1. **API 是最大的风险**。存储引擎可以重构,公开 API 一旦发布就难改。
   在最简单的载体(内存)上把 API 打磨冻结,成本最低;
2. **它本身是可用产品**:单测、嵌入式场景的临时记忆、向量缓存;
3. 后续每层都拿内存层当**正确性参照**:HNSW 的结果必须与暴力扫描一致(统计意义上),
   持久化的恢复结果必须与内存等价。

---

## 2. 公开 API 语义细则(冻结)

### 2.1 写入:`insert` 与 `insert_batch`

```rust
pub fn insert(&self, rec: Record) -> Result<InsertOutcome>;
pub fn insert_batch(&self, recs: Vec<Record>) -> Result<Vec<InsertOutcome>>;
pub enum InsertOutcome { Inserted(RowId), Duplicate { existing: RowId, score: f32 } }
```

| 规则 | 语义 |
|---|---|
| 维度校验 | 向量长度 ≠ 建库维度 → `DimensionMismatch`(故 `Record::new` 不返回 `Result`) |
| 数值校验 | 任一分量为 `NaN`/`±Inf` → `Invalid`(否则污染 `Metric::better` 排序;见 [11 §8](11-api-reference.md)) |
| 同 key | `InsertMode::Upsert`(默认):分配**新** RowId,旧行打墓碑;`RejectDuplicate`:报错 |
| seqno | 每次成功 insert 分配新 `SeqNo`,全库单调递增 |
| TTL | 相对时长即刻换算为绝对 `expires_at`(Unix 毫秒,经 `Clock` 取值) |
| importance | 未指定默认 0.5;范围 [0,1],超范围钳制(clamp) |
| 批量 | `insert_batch` **整批原子**(I15):共用一次组提交 fsync;WAL 侧以 `BatchBegin`/`BatchCommit` 帧包裹([04 §2.3](04-l2-persist.md));任一条校验失败则整批拒绝、不产生部分写入;结果顺序与输入一一对应 |

### 2.2 读取:`search` 与 `get`

```rust
pub struct SearchBuilder<'a> { /* 链式:vector / text / top_k / ef / filter / dedup / fusion */ }
impl SearchBuilder<'_> { pub fn execute(&self) -> Result<Vec<Hit>>; }
```

- `top_k` 默认 10,上限 4096;`ef` 仅在 L3 后生效(内存层忽略);
- `Hit` 按 `Metric::better` 排序(最优在前);同分按 `RowId` 升序(**排序全等性**:同一快照内任意两次查询结果完全一致);
- 过滤语义:`filter` 在打分**之前**限定候选集(预过滤),不是"取完再筛"
  ——预过滤保证 top_k 是"过滤后的前 k",这是 Agent 记忆的正确语义
  ("只要重要度 > 0.5 的前 10 条",而不是"前 10 条里有几条重要的算几条");
- `get(key)` / `get_by_rowid`:快照一致的单点读,返回 `RecordRef`(无 score,因为没有查询);
  原始向量经 `RecordRef::vector()` 或 `get_vector(rowid)` 取回([11 §1.2](11-api-reference.md))。

### 2.3 生命周期:`touch / forget / delete / iter`

- `touch(key, boost)` / `touch_by_rowid(rowid, boost)`:访问计数 +1、`last_access = now`
  (为 [07 遗忘曲线](07-l5-life.md) 供数);`boost` 为 `Some(d)` 时同时提升 importance
  (`importance += d`,clamp 到 [0,1]);按 rowid 的版本用于无 key 记录;
- `delete(key)` / `delete_by_rowid(rowid)`:墓碑;`forget(filter)`:对过滤器命中的每行打墓碑,返回删除数;
- `iter(filter)`:按过滤条件流式遍历(导出/审计/重建用),快照一致、不参与 ANN;
  逐行返回 `Result<RecordRef>`——迭代中途的 I/O 错误必须能被调用方看到([11 §1.3](11-api-reference.md))。

### 2.4 落盘与关闭:`flush / close`(在 `Mneme` 上)

```rust
impl Mneme {
    pub fn flush(&self) -> Result<()>;   // 把内存可变表写成不可变段 + fsync WAL
    pub fn close(self) -> Result<()>;    // flush + 释放文件锁;幂等(对已关闭的库经其他句柄再调返回 Ok)
}
```

- `flush` 是 `FsyncPolicy::OnFlush` 的显式触发点,也是备份/compaction 的一致性点;
- `close` 返回 `Ok` 即代表所有已确认写入持久(I16);`Drop` 只**尽力** `flush`
  并忽略错误,需要确保持久性时必须显式 `close`。崩溃场景的恢复见
  [04 §7](04-l2-persist.md),用户侧 runbook 见 [11 §7](11-api-reference.md)。

---

## 3. 内存表结构:`table.rs`

```text
Table
├── writer:  Mutex<WriterState>          ← 写路径全局串行
├── reader:  RwLock<ReaderView>          ← 读路径短暂持读锁
└── config:  Arc<Config>                 (dimension / metric / 默认去重阈值…)
WriterState
├── vectors:   Vec<AlignedVec<f32>>      ← 下标 = SlotId,32B 对齐
├── entries:   Vec<Option<Entry>>        ← None = 从未使用/已物理清出
├── key_index: HashMap<(NsId, Key), SlotId>
├── dead:      BitSet                    ← 墓碑位图(按 SlotId)
├── seqno:     SeqNo                     ← 下一个可分配序号
├── next_rowid: RowId                    ← 全局记录标识水位(L2 起持久化)
└── access:    HashMap<RowId, AccessStat> ← touch 累积区(L5 落盘;RowId 稳定,跨 compaction 不失效;类型见 [11 §1.6](11-api-reference.md))
Entry { rowid, key, text, meta, created_at, expires_at, importance, seqno, last_access, access_count }
```

- `vectors` 与 `entries` 的下标即 `SlotId`,**永不回收**——`Vec` 只增不减,
  墓碑槽位保留占位(每行 4 字节空向量指针 + Option 枚举 24 字节级开销,可控);
  `SlotId → RowId` 的映射即 `Entry.rowid`;
- 读者先拿 `RwLock` 读锁**复制出扫描所需的视图信息**(活行数、当前 `dead` 位图引用、
  vectors 切片),随即释放锁再扫描——写者等待读者的时间只有"复制视图"的纳秒级,
  而不是整个扫描。此简化依赖一个事实:暴力扫描 O(N·d) 毫秒级,做 COW 版本管理得不偿失;
   L2 引入段结构后,同样的模式升级为"ReaderView 视图 + 不可变段"(见 [04 §8](04-l2-persist.md))。

---

## 4. 暴力扫描:`search.rs`

### 4.1 【直觉】图书馆逐本翻

没有索引时,"想起"只能把每本藏书翻一遍(计算 q 与每个向量的相似度),
拿个只有 k 个格子的小托盘([02 §5 TopK](02-l0-core.md)),比托盘里最差的好的才放进去。
简单、精确、没有维护成本——数据少时,这就是**正确**的选择。

### 4.2 【算法】流程(含过滤与并行)

```text
输入: q, top_k, filter(AST), 有效快照视图
1. 评估 filter 的快速形态: 若为 Always → 全量活行; 否则先逐行求值?
   → 优化: 先做"元数据扫描"(每行仅求值 Expr, 便宜), 得到候选位图 cand
2. 将活行 ∩ cand 按 8192 行一块切分, 分给 scoped threads   # 8192 = 计算并行粒度
3. 每线程: 遍历本块行 → simd::dot(q, v[i]) (± norm 归一) → 块内 TopK(k)
4. k 路归并各块 TopK → 全局 TopK → 按 Score 降序、RowId 升序输出
```

要点:**过滤先行**。第 1 步只评估元数据(不碰向量,不进 SIMD 通道),
得到候选位图后再算分数——被过滤掉的行零向量读取,这正是 L4 计划器的雏形
(在 L4 升级为 zone map/bloom 块级剪枝,见 [06 §2](06-l4-query.md))。

> **两个"块"不要混淆**:这里的 **8192 行**是**计算并行粒度**(线程分块);
> [04 §2.1](04-l2-persist.md) 与 zone map 的 **1024 行**是**存储/索引块粒度**
> (删除位图、min/max 摘要)。二者独立,可各自调参。

### 4.3 【复杂度】

| 阶段 | 时间 | 空间 |
|---|---|---|
| 元数据过滤求值 | $O(N \cdot \|E\|)$,$\|E\|$ = AST 节点数 | $O(N/8)$ 位图 |
| 向量打分 | $O(N_c \cdot d)$,$N_c$ = 候选数(无过滤则 $N$) | $O(C_{\text{块}} \cdot k)$ |
| 归并 | $O(C_{\text{块}} \cdot k \log k)$ | $O(C_{\text{块}} \cdot k)$ |

**带宽视角**(并行扩展性的物理上限):每查询读 $4 d$ 字节/行,
1M×1536 维 ≈ 6 GB;双通道内存 ~50 GB/s → **纯暴力下限约 120ms**。
这解释了:① 为什么 L3 必须 HNSW(只读 ef×M 个向量,~万分之一);
② 为什么 [08 量化](08-l6-quant.md)(字节 ÷4)与 mmap(页缓存命中)重要。

### 4.4 【算例】

4 行,`q=[1,1]`,filter `importance > 0.5`:

```
r0 [1,0] imp=0.9 → 候选; r1 [0,1] imp=0.2 → 剔除(不读向量)
r2 [1,1] imp=0.7 → 候选; r3 [2,0] imp=0.8 → 候选
打分: r0=1, r2=2, r3=2  → top2 = [r2, r3](同分 2,RowId 升序)✓
```

---

## 5. 过滤 AST:`pred.rs`

### 5.1 AST 定义(与 L4 共用,此处定型)

```rust
pub enum Expr {
    Cmp { op: CmpOp, field: String, val: Val },   // == != > >= < <=
    In(String, Box<[Val]>),   // 有序、去重的取值集合(构造时排序;f64 非 Ord,故不用 BTreeSet)
    And(Box<[Expr]>), Or(Box<[Expr]>), Not(Box<Expr>),
    Always, Never,
}
pub enum Val { Bool(bool), Int(i64), Num(f64), Str(Arc<str>), Ts(i64) }  // Ts = Unix 毫秒
```

- L1 提供 builder 组合器(`Expr::field("importance").gt(0.5)`、`&`/`|` 运算符重载);
- **字符串解析器与 JSON 往返在 L4**([06 §1](06-l4-query.md)),AST 不变——
  这是"接口先于实现"的又一例;
- 类型规则:数值比较时 Int/Num 互通;`Ts` 只与 `Ts` 比较(时间语义明确化);
  字段缺失 → 比较结果为 false(不报错,记忆元数据是开放 schema,缺字段是常态)。

### 5.2 求值复杂度

单行求值 $O(|E|)$;短路求值:`And` 左支 false 即停,`Or` 左支 true 即停——
AST 构造时**把高选择性条件放左边**(L4 计划器自动做重排,见 [06 §2](06-l4-query.md))。

---

## 6. 去重预检:`dedup.rs`(位于 memory/,L4 挂接元数据索引)

### 6.1 【直觉】"这话我记过吗?"

Agent 会反复遇到相似信息("用户又说了他喜欢深色模式")。
无脑全记 → 记忆库被重复淹没;所以插入前先问一句库:
"有没有一条和它**几乎一样**的记忆?"——用向量相似度本身来判重:
插入向量的 top-1 邻居若相似度 ≥ 阈值(默认 cosine 0.95),视为重复。

### 6.2 两级判重

| 级别 | 手段 | 成本 | 语义 |
|---|---|---|---|
| 精确 | 文本 FNV-1a 64 位哈希集合 | $O(1)$ | 文本完全相同 → 必重复 |
| 近似 | top-1 向量查询 | 一次检索 | 相似度 ≥ 阈值 → 重复 |

**FNV-1a(64 位)**:`hash = 0xcbf29ce484222325`(偏移基);
对每个字节 `hash ^= b; hash *= 0x100000001b3`(FNV 素数)。
逐字节两次整数运算,分布均匀、无碰撞对抗需求(哈希碰撞只导致多查一次向量,无害)。

### 6.3 重复的处理策略

```rust
pub enum Dedup { Off, Reject, Replace, KeepBoth, Merge(fn(&RecordRef, &RecordRef) -> Option<Record>) }
```

近似判重的余弦阈值由 `Builder::dedup_threshold(f32)` 配置(默认 0.95,
见 [11 §2](11-api-reference.md)),与检索结果的 `ResultDedup::Near { threshold }` 各自独立;
`RecordRef` 定义见 [11 §1.2](11-api-reference.md)。

- `Reject`:返回 `InsertOutcome::Duplicate { existing, score }`,由调用方决定;
- `Replace`:旧行墓碑,新行入位(保留新时间戳);
- `KeepBoth`:照常插入(调用方只想**知道**有重复);
- `Merge`:回调合并(如"保留旧记忆 + 更新其 last_access"),L4 之后可用元数据索引细化
  (如只在 `kind == "preference"` 内查重——把判重查询限定在同一语义类别,防误杀)。

> **注意**:`Dedup` 是**写入期**去重。检索结果的去重是另一套语义与另一个类型
> `ResultDedup`(`Off / ById / Near { threshold }`,见 [06 §6](06-l4-query.md)),
> 只作用于单次 `execute()` 的命中列表,不要混用。

**判重的复杂度**:精确级 $O(1)$;近似级 = 一次 top-1 检索(L1 暴力 $O(Nd)$,
L3 后 $O(ef \cdot M_0 \cdot d)$,可忽略)。**去重是 Agent 记忆库"不膨胀"的第一道闸门**,
与 compaction([07](07-l5-life.md))的正向清理互补。

---

## 7. 并发模型(L1 定型,L2 沿用)

| 角色 | 机制 | 说明 |
|---|---|---|
| 写者 | `Mutex<WriterState>` | 全局串行;单条写入微秒级,无需分片 |
| 读者 | `RwLock<ReaderView>` 短读锁 → 拷贝视图 → 无锁扫描 | 见 §3 末尾 |
| 快照一致性 | 读锁内取 `(seqno 水位, dead 位图, vectors)` 一次成型 | 同快照内多次查询结果全等 |
| 并行扫描 | `std::thread::scope`(无 rayon) | 作用域线程保证借用安全,零生命周期泄漏;线程数默认取 `std::thread::available_parallelism()` |

**不承诺**:跨快照的可重复读(拿新快照自然看见新写入);这是嵌入式库的合理语义,
Agent 框架层若需事务语义由调用方组织。

**公开类型的线程安全**:`Mneme`/`Namespace`/`SnapshotHandle` 均 `Send + Sync`,
内部 `Arc` 克隆廉价,可跨线程共享;逐类型保证见 [11 §5](11-api-reference.md)。

---

## 8. 层边界契约(L1 → 上层)

**向上冻结**(= 公开 API,见 [01 §6](01-overview.md)、[11](11-api-reference.md) 与本章 §2):
`Mneme / Namespace / Record / Hit / RecordRef / SearchBuilder / InsertOutcome / Expr / Dedup / ResultDedup / Retention 语义`,
以及 `insert_batch / get / get_by_rowid / get_vector / count / delete / delete_by_rowid /
touch / touch_by_rowid / iter / flush / close / list_namespaces / drop_namespace /
snapshot / backup_to / stats / check` 的签名。

**向下(L0)**:只使用 [02 §9](02-l0-core.md) 契约内的类型与函数。

**向 L2 交接的内部接口**(L2 必须实现,以便内存层无痛升级为持久层):

```rust
// SearchOpt: 检索参数打包(top_k / ef / filter / dedup / fusion);
// EntryRef:  段内记录只读视图(公开 `RecordRef` 的内部形态,额外带 SlotId/seqno)。
pub trait VectorStore: Send + Sync {
    fn insert_batch(&self, batch: &[Record]) -> Result<Vec<InsertOutcome>>;
    fn delete(&self, keys: &[Key]) -> Result<usize>;
    fn search(&self, q: &[f32], opt: &SearchOpt) -> Result<Vec<Hit>>;
    fn scan_alive(&self) -> impl Iterator<Item = (RowId, &EntryRef)>;
}
```

L2 的 `Database` 即 `VectorStore` 的持久实现;L3 的 HNSW 再替换其 `search` 实现——
公开 API 始终不变。

## 下一章

[04-l2-persist.md](04-l2-persist.md):把内存中的世界搬到磁盘上,并且保证断电不丢。
