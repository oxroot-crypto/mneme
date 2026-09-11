# 03 L1 内存引擎:公开 API 在此冻结

> **本章目标**:在全内存中实现**完整的公开 API**——还没有持久化、还没有索引,
> 但一个 Agent 需要的"记住/想起/遗忘"已经全部可用。
> **前置阅读**:[01 §6](01-overview.md)(API 速览)、[02](02-l0-core.md)。
> **本章你将学到**:API 语义细则 → 内存表结构 → 暴力扫描 → 过滤 AST → 去重预检。

模块:`memory/{engine.rs, engine_ops.rs, builder.rs, namespace/, snapshot.rs, snapshot_scan.rs,
search_builder.rs, search_exec.rs, expand.rs, rerank.rs, table/(mod,handle,state,view,write_op).rs, index.rs, search.rs, pred.rs,
pred_eval.rs, record.rs, write_helpers.rs, mutate_helpers.rs, dedup.rs, relation.rs,
temporal.rs, score.rs, lifecycle.rs, ops.rs, config.rs}`——`engine.rs` 承载库句柄 `Mneme`
(统计/fsck/落盘门面在 `engine_ops.rs`),`namespace/` 承载 `Namespace` 的写/读/访问/
生命周期/关系方法(过滤遍历与计数在 `namespace/scan.rs`),`snapshot.rs`/`snapshot_scan.rs`
承载快照只读视图;`search_builder.rs`、`search_exec.rs` 与 `expand.rs` 承载 `SearchBuilder`
执行流程及联想扩展/结果去重;`index.rs` 是 L3 索引替换缝(内部 trait,公开 API 不变);
原 `bitset.rs` 已上移 L0 `core/bitset.rs`(构建期/查询期共用位图);本章 §2 的语义即其
行为规约,其余小节逐个展开数据结构与算法。

---

## 1. 为什么先做内存层(渐进式理由)

1. **API 是最大的风险**。存储引擎可以重构,公开 API 一旦发布就难改。
   在最简单的载体(内存)上把 API 打磨冻结,成本最低;
2. **它本身是可用产品**:单测、嵌入型场景的临时记忆、向量缓存;
3. 后续每层都拿内存层当**正确性参照**:HNSW 的结果必须与暴力扫描一致(统计意义上),
   持久化的恢复结果必须与内存等价。

---

## 2. 公开 API 语义细则(冻结)

### 2.1 写入:`insert` 与 `insert_batch`

```rust
pub fn insert(&self, rec: Record) -> Result<InsertOutcome>;
pub fn insert_batch(&self, recs: Vec<Record>) -> Result<Vec<InsertOutcome>>;
pub enum InsertOutcome { Inserted(RowId), Merged(RowId), Duplicate { existing: RowId, score: f32 } }
```

| 规则 | 语义 |
|---|---|
| 维度校验 | 向量长度 ≠ 建库维度 → `DimensionMismatch`(故 `Record::new` 不返回 `Result`) |
| 数值校验 | 任一分量为 `NaN`/`±Inf` → `NonFinite`(否则污染 `Metric::better` 排序;见 [16 §8](16-api-reference.md)) |
| 同 key | `InsertMode::Upsert`(默认):**保留既有 RowId**,写入新物理版本(seqno+1),旧版本被遮蔽;`RejectDuplicate`:同 key 存在**可见**记录(未墓碑且未逻辑过期)时报错,墓碑/逻辑过期记录视为不存在并复用既有 RowId。RowId 因此是跨更新稳定的逻辑身份(02 §1) |
| seqno | 每次成功写入分配新 `SeqNo`,全库单调递增 |
| TTL | 相对时长即刻换算为绝对 `expires_at`(Unix 毫秒,经 `Clock` 取值) |
| importance | 未指定默认 0.5;范围 [0,1],超范围钳制(clamp);含非有限值(NaN)→ `NonFinite` 拒绝,绝不入库 |
| valid time | `Record::valid_from/valid_to` 可选,构成双时态(valid time + transaction time),见 [09 §3](09-memory-model.md) |
| confidence | `Record::confidence` 可选,默认 1.0;参与检索打分的可信度因子,见 [10](10-scoring.md) |
| 事务性 | 任何写操作(单条/批量/update/delete/touch/feedback/forget/retain/consolidate/drop_namespace…)失败都回滚到操作前状态、对读者不可见,副作用(命名空间登记、访问计数、关系边)一并回滚,绝不半写(FC-MEM-POST-002) |
| 批量 | `insert_batch` **整批原子**(I15,定义见 [16 §9](16-api-reference.md)):共用一次组提交 fsync;WAL 侧以 `BatchBegin`/`BatchCommit` 帧包裹([04 §2.3](04-l2-persist.md));任一条**校验失败**(维度/数值/限额)则整批拒绝;预校验后逐条求值仍失败(`Dedup::Merge` 回调产物超限、槽位容量溢出)时同样整批回滚、不产生部分写入;结果顺序与输入一一对应。**去重命中**(`Dedup::Reject`)或 `InsertMode::RejectDuplicate` 属于逐条业务结果,不使整批回滚——命中位置返回 `Duplicate`,其余记录照常写入 |

**局部更新 `update`**(不改变 RowId;不提供向量则不写向量区):

```rust
pub fn update(&self, key: &str, patch: UpdatePatch) -> Result<UpdateOutcome>;
pub fn update_by_rowid(&self, id: RowId, patch: UpdatePatch) -> Result<UpdateOutcome>;
pub enum UpdateOutcome { Updated(RowId), NotFound }
```

| 字段 | 语义 |
|---|---|
| `vector` | 提供时替换向量并重建该版本索引;不提供则保留 |
| `text` / `metadata` / `provenance` | 提供时整体替换(非合并);`Some(None)` 清空;**受 [16 §8](16-api-reference.md) 限额约束**(与 insert 同口径),超限返回 `TooLarge`/`MetaTooDeep` 且记录保持上一版本原样 |
| `importance` / `ttl` / `valid_time` / `confidence` | 提供时覆盖;`ttl(Some(None))` 取消过期;`importance`/`confidence` 含非有限值(NaN)→ `NonFinite` |
| 可见性 | **不变量 I24(更新原子可见)**:更新写入新物理版本(新 seqno),对读者**原子可见**——任一并发查询要么看到旧版本、要么看到新版本,绝不看到字段混合的半更新;旧版本遮蔽,超期版本由 compaction 按 `history_horizon` 物理回收(I26) |
| 与 upsert 的区别 | upsert 按 key 整体替换、可无既有 key;update 要求已存在,返回 `NotFound` 而非新建 |

### 2.2 读取:`search` 与 `get`

```rust
pub struct SearchBuilder<'a> { /* 链式:vector / text / top_k / ef / filter / dedup / fusion /
                                 score / diversify / expand / as_of / query_id / rerank */ }
impl SearchBuilder<'_> { pub fn execute(&self) -> Result<Vec<Hit>>; }
```

- `top_k` 默认 10,上限 4096;`ef` 仅在 L3 后生效(内存层忽略);
- 查询向量长度必须等于建库维度,否则 `execute()` 返回 `DimensionMismatch`(与写入校验一致);
- `Hit` 按**最终打分**(默认纯相似度,或 [10](10-scoring.md) 的综合分)排序,最优在前;
  同分按 `RowId` 升序(**排序全等性**:同一快照内任意两次查询结果完全一致);
- 过滤语义:`filter` 在打分**之前**限定候选集(预过滤),不是"取完再筛"
  ——预过滤保证 top_k 是"过滤后的前 k",这是 Agent 记忆的正确语义
  ("只要重要度 > 0.5 的前 10 条",而不是"前 10 条里有几条重要的算几条")。
  L3 起 ANN 为保召回会先放大 `ef` 再在结果集上过滤([05 §8](05-l3-hnsw.md) 档①),
  那是**性能策略**;其语义仍严格等价于"候选位图内暴力",与预过滤不矛盾;
- `score(Scoring)` 开启时序/重要度/访问感知排序;`diversify(Diversity)` 开启 MMR;
  `expand(RelationExpand)` 沿关系边做联想扩展;`as_of(ts)` 做双时态历史读(在版本链上取
  事务时间 ≤ ts 的可见版本)——四者语义见 [10](10-scoring.md) 与 [09](09-memory-model.md);
- 点读家族(快照一致):
  - `get(key)` / `get_by_rowid(id) -> Result<Option<RecordRef<'_>>>`;
  - `get_many(keys: &[&str]) -> Result<Vec<Option<RecordRef<'_>>>>`,
    `get_many_by_rowid(ids: &[RowId]) -> ...`——一次持锁、批量定位,避免 N 次往返;
  - `exists(key) -> Result<bool>`;
  - `count(filter: Option<Expr>) -> Result<u64>`:统计命中的活记录数,**不物化记录**、复用过滤器/索引;
    过滤语义与 `search` 一致(预过滤),墓碑与逻辑过期记录不计入(I9);
  - `get_vector(id) -> Result<Option<Vec<f32>>>`(只取向量,不物化记录)。
- **`RecordRef<'_>` 以 `Arc` 持有物理版本**(见 [16 §1.2](16-api-reference.md)):`vector()`
  返回 `&[f32]` 零拷贝(仅 `Arc` 引用计数,不复制向量字节),字段经访问器读取;
  `iter` 同样不物化向量,需要向量时显式 `get_vector`。这是 L1 在安全 Rust 下满足
  `get() -> RecordRef<'_>` 签名的实现方式;L2 段文件 + mmap 落地后,`Arc` 内部指向
  不可变段数据,签名与调用方均不变。

### 2.3 生命周期:`touch / forget / delete / iter`

- `touch(key, boost)` / `touch_by_rowid(rowid, boost)`:访问计数 +1、`last_access = now`
  (为 [07 遗忘曲线](07-l5-life.md) 供数);`boost` 为 `Some(d)` 时同时提升 importance
  (`importance += d`,clamp 到 [0,1];`d` 为非有限值(NaN)→ `NonFinite`);按 rowid 的版本用于无 key 记录;
  **仅对可见记录生效**:不存在/已墓碑/已逻辑过期 → 返回 `false` 且不计访问统计
  (与读路径/`feedback` 同口径,FC-MEM-POST-009);
- `delete(key)` / `delete_by_rowid(rowid)`:墓碑;`forget(filter)`:对过滤器命中的每行打墓碑,返回删除数;
- `iter(filter)`:按过滤条件遍历(导出/审计/重建用),快照一致、不参与 ANN;
  逐行返回 `Result<RecordRef<'_>>`——迭代中途的 I/O 错误必须能被调用方看到([16 §1.3](16-api-reference.md));
- `feedback(rowid, Feedback, query_id)`:检索反馈闭环([10 §4](10-scoring.md)),把"这条记忆是否被
  采用/纠正"回写为访问增益或重要度修正;幂等键 `(rowid, query_id)` 防重复计分
  (`query_id` 由 `execute()` 生成并随 `Hit` 返回,见 [10 §4.2](10-scoring.md));
  对不可见记录(不存在/已墓碑/已过期)返回 `false` 且不占用幂等键;
- `relate(from, to, kind, weight)` / `relate_with_options(from, to, RelateOptions)` /
  `unrelate(...)`:建立/删除记忆关系边
  ([09 §2](09-memory-model.md));关系边随记录墓碑级联失效;
- `consolidate(policy)`:对满足过滤的近似重复记忆做聚类→合并/摘要→链接来源
  ([09 §5](09-memory-model.md)),是 episodic→semantic 沉淀的引擎原语。

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
  [04 §7](04-l2-persist.md),用户侧 runbook 见 [16 §7](16-api-reference.md);
- **克隆与关闭**:`Mneme` 克隆共享同一底层库;`close(self)` 关闭的是**共享库**而非单个
  句柄——首个 `close` 完成 `flush` 并释放文件锁,此后所有克隆(及其派生的 `Namespace`)
  的读写返回 `Closed`,再次 `close` 返回 `Ok`(幂等)。因此不要让克隆存活到
  `close` 之后。

---

## 3. 内存表结构:`table/`

```text
Table
├── writer:  Mutex<WriterState>          ← 写路径全局串行
├── reader:  RwLock<Arc<ReaderView>>     ← 读路径短暂持读锁
└── config:  Arc<Config>                 (dimension / metric / 默认去重阈值…)
WriterState
├── slots:      Arc<Vec<Arc<SlotData>>>  ← 下标 = SlotId(物理版本),只增不减
├── dead:       Arc<BitSet>              ← 当前不可见版本位图(按 SlotId;被遮蔽/删除;as_of 仍可读)
├── key_index:  Arc<HashMap<(NsId, Key), RowId>>  ← key → 稳定 RowId
├── text_index: Arc<HashMap<(NsId, u64), RowId>>  ← 文本 FNV-1a 哈希 → RowId(精确去重)
├── versions:   Arc<HashMap<RowId, Vec<SlotId>>>  ← 版本链(按 seqno 升序,含墓碑版本;L2 持久化)
├── latest:     Arc<HashMap<RowId, SlotId>>       ← 每个 RowId 当前可见版本
├── out_edges / in_edges: Arc<HashMap<RowId, Vec<Edge>>>  ← 关系邻接表(双向)
├── access:     Arc<HashMap<RowId, AccessStat>>   ← touch 累积区(L5 落盘)
├── seqno:      SeqNo                    ← 下一个可分配序号
├── next_rowid / next_ns_id: u64 / u32   ← 标识水位
├── ns_registry / ns_by_path: Arc<HashMap<…>>     ← 命名空间路径 ↔ NsId 双向注册表
├── feedback_seen: Arc<HashSet<(RowId, u64)>> ← 反馈幂等键(I27)
└── closed:     bool                     ← 关闭标记
SlotData { rowid, ns_id, ns_path, seqno, key, vector: Arc<[f32]>, norm_sq,
           text, text_hash, meta, created_at, expires_at, importance, confidence,
           valid_from, valid_to, provenance, tx_ms, deleted }
ReaderView = WriterState 的不可变快照(仅克隆 Arc 句柄):slots / dead / key_index /
             versions / latest / out_edges / in_edges / access / ns_registry / seqno / closed
```

- **一个 RowId 是一条版本链**:`versions[RowId]` 按 seqno 升序保存全部保留版本(记录体或墓碑),
  `latest` 指向当前可见版本;读取时按"`seqno ≤ W 且 tx_ms ≤ T` 的最新版本"解析可见性
  ([04 §2.2](04-l2-persist.md)),`as_of(T)` 即取历史水位;超期版本由 compaction 回收;`get_by_rowid` 先查 `latest`;
- **`dead` 位图**:被遮蔽/删除的物理槽位置位;`as_of` 重建视图时仍可经版本链读取历史,
  `SlotData.deleted` 标记墓碑版本本身;
- **关系边**:`out_edges`/`in_edges` 双向邻接表,以 `(from, to, kind)` 为唯一键;flush 时并入新段
  relations 区([04 §2.2b](04-l2-persist.md)),compaction 时物化([09 §2](09-memory-model.md))。

- `slots` 的下标即 `SlotId`,**永不回收**——`Vec` 只增不减,
  墓碑槽位保留占位(每槽 = `Arc<SlotData>` 句柄 + 记录体,约百字节级),
  随墓碑比例线性增长,由 compaction 物理回收;
  `SlotId → RowId` 的映射即 `SlotData.rowid`;
- 读者先拿 `RwLock` 读锁**克隆 `Arc<ReaderView>`**,随即释放锁再扫描——写者等待读者的时间
  只有"克隆视图句柄"的纳秒级,而不是整个扫描。此简化依赖一个事实:暴力扫描 O(N·d) 毫秒级,
  做 COW 版本管理得不偿失;
    L2 引入段结构后,同样的模式升级为"ReaderView 视图 + 不可变段"(见 [04 §8](04-l2-persist.md))。

### 3.1 版本状态机(FC-MODEL-STA-001)

一个 `RowId` 的物理版本在生命周期内只经历三种状态,形式化五元组:

```text
M = (S, E, δ, s0, F)
S = { Active, Shadowed, Reclaimed }
E = { Update, Upsert, Delete, AsOf, Compact }
δ(Active, Update|Upsert|Delete) = Shadowed(旧版本) ∧ Active(新版本 / 无)
δ(Shadowed, AsOf)               = Shadowed(历史可见,仅 as_of)
δ(Shadowed, Compact)            = Reclaimed(物理回收,history_horizon 外)
s0 = Active
F  = { Reclaimed }
```

- **合法转移**:`Active → Shadowed`(更新/删除)、`Shadowed → Reclaimed`(compaction);
- **非法转移**:`Shadowed` 出现在当前读路径(`get`/`search`/`iter`/`count`)——必须不可见并
  显式拦截,绝不静默返回;`Reclaimed` 不可再被任何读路径访问。L1 无 compaction,`Reclaimed`
  不出现;回收语义随 L2/L5 落地。
- **复杂度验证口径**(§9.3):L1 无 criterion 基准,CPLX 以「操作计数单测 + 解析证明」验证
  (见 [spec/contracts.md §9.2.2](../spec/contracts.md)),基准自 L3 起引入。

---

## 4. 暴力扫描:`search.rs`

### 4.1 【直觉】图书馆逐本翻

没有索引时,"想起"只能把每本藏书翻一遍(计算 q 与每个向量的相似度),
拿个只有 k 个格子的小托盘([02 §5 TopK](02-l0-core.md)),比托盘里最差的好的才放进去。
简单、精确、没有维护成本——数据少时,这就是**正确**的选择。

### 4.2 【工程】流程(含过滤与并行)

```text
输入: q, top_k, filter(AST), 有效快照视图
1. 评估 filter 的快速形态: 若为 Always → 全量活行; 否则先逐行求值
   → 优化: 先做"元数据扫描"(每行仅求值 Expr, 便宜), 得到候选位图 cand
2. 将活行 ∩ cand 按 8192 行一块切分, 分给 scoped threads   # 8192 = 计算并行粒度
3. 每线程: 遍历本块行 → simd::dot(q, v[i]) (± norm 归一) → 块内 TopK(k)
4. k 路归并各块 TopK → 全局 TopK → 按 `Metric::better` 排序(最优在前)、同分按 RowId 升序输出
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
| 向量打分 | $O(N_c \cdot d)$,$N_c$ = 候选数(无过滤则 $N$) | $O(C_{\text{block}} \cdot k)$ |
| 归并 | $O(C_{\text{block}} \cdot k \log k)$ | $O(C_{\text{block}} \cdot k)$ |

**带宽视角**(并行扩展性的物理上限):每查询读 $4 d$ 字节/行,
1M×1536 维 ≈ 6 GB;双通道内存 ~50 GB/s → **纯暴力下限约 120ms**。
这解释了:① 为什么 L3 必须 HNSW(只读约千个向量,~1/1000);
② 为什么 [08 量化](08-l6-quant.md)(字节 ÷4)与 mmap(页缓存命中)重要。

### 4.4 【算例】

4 行,`q=[1,1]`,filter `importance > 0.5`:

```
r0 [1,0] imp=0.9 → 候选; r1 [0,1] imp=0.2 → 剔除(不读向量)
r2 [1,1] imp=0.7 → 候选; r3 [2,0] imp=0.8 → 候选
打分: r0=1, r2=2, r3=2  → top2 = [r2, r3](同分 2,RowId 升序)✓
```

---

## 5. 过滤 AST 与求值:`pred.rs` / `pred_eval.rs`

### 5.1 AST 定义(与 L4 共用,此处定型)

```rust
pub enum CmpOp { Eq, Ne, Gt, Ge, Lt, Le }         // == != > >= < <=
pub enum Expr {
    Cmp { op: CmpOp, field: String, val: Val },
    In(String, Box<[Val]>),   // 有序、去重的取值集合(构造时排序;f64 非 Ord,故不用 BTreeSet)
    Contains(String, Val),    // 数组含元素 / 字符串含子串
    StartsWith(String, Arc<str>), EndsWith(String, Arc<str>),
    Glob(String, Arc<str>),   // 通配符:* 任意串,? 单字符(不是正则,零依赖)
    Exists(String),           // 字段存在(可为 null)
    IsNull(String),           // 字段存在且为 JSON null
    And(Box<[Expr]>), Or(Box<[Expr]>), Not(Box<Expr>),
    Always, Never,
}
pub enum Val { Bool(bool), Int(i64), Num(f64), Str(Arc<str>), Ts(i64) }  // Ts = Unix 毫秒

/// 组合器:`Expr::field("importance").gt(0.5)`;`in` 是关键字,故集合判定方法名用 `is_in`。
pub struct FieldBuilder { /* field: String */ }
impl FieldBuilder {
    pub fn eq(self, v: impl Into<Val>) -> Expr;
    pub fn ne(self, v: impl Into<Val>) -> Expr;
    pub fn gt(self, v: impl Into<Val>) -> Expr;
    pub fn ge(self, v: impl Into<Val>) -> Expr;
    pub fn lt(self, v: impl Into<Val>) -> Expr;
    pub fn le(self, v: impl Into<Val>) -> Expr;
    pub fn is_in(self, vs: impl IntoIterator<Item = Val>) -> Expr;
}
```

- L1 提供 builder 组合器(`Expr::field("importance").gt(0.5)`、`&`/`|` 运算符重载);
- **字符串解析器与 JSON 往返在 L4**([06 §1](06-l4-query.md)),AST 不变——
  这是"接口先于实现"的又一例;
- 类型规则:数值比较时 Int/Num 互通;`Ts` 只与 `Ts` 比较(时间语义明确化);
  `Contains`/`StartsWith`/`EndsWith`/`Glob` 要求字符串或数组;`Exists`/`IsNull` 只判存在性;
- **三值语义**:字段缺失时 `Cmp`/`In`/`Contains`/`StartsWith`/`EndsWith`/`Glob` 求值为 false,
  且 `Not(false)` 仍为 false——即 `not(kind == "x")` **不会**命中无 `kind` 字段的记录;
  查"字段缺失"用 `Exists`(记忆元数据是开放 schema,缺字段是常态);

### 5.2 求值复杂度

求值器位于 `pred_eval.rs`(AST 与组合器在 `pred.rs`)。
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
pub enum Dedup { Off, Reject, Replace, KeepBoth, Merge(fn(&RecordRef<'_>, &RecordRef<'_>) -> Option<Record>) }
```

近似判重的阈值由 `Builder::dedup_threshold(f32)` 配置(默认 0.95,
`[0,1]` 内的有限值,越界建库即拒绝;见 [16 §2](16-api-reference.md)),与检索结果的 `ResultDedup::Near { threshold }` 各自独立。
**该阈值统一按余弦相似度口径**:非余弦度量(`Dot`/`Euclidean`)下引擎先把两侧向量归一化
再比较,调用方无需换算。
`RecordRef` 定义见 [16 §1.2](16-api-reference.md)。

- `Reject`:返回 `InsertOutcome::Duplicate { existing, score }`,由调用方决定;
- `Replace`:旧行墓碑,新行入位(保留新时间戳);
- `KeepBoth`:照常插入,返回 `Inserted(RowId)`(不返回重复信息;需感知重复请用 `Reject`);
- `Merge`:以命中的旧记录为主体应用回调返回的 `Record`(如保留旧向量、取更高 `importance`、合并 `text`),
  **保留旧 RowId 就地更新**(符合 I22),返回 `InsertOutcome::Merged(existing)`;回调返回 `None` 等价 `KeepBoth`;
  若回调产物改变了 key,`key_index` 随新版本迁移(旧 key 映射移除,不悬挂)。
  L4 之后可用元数据索引细化(如只在 `kind == "preference"` 内查重——把判重查询限定在同一语义类别,防误杀)。

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
| 读者 | `RwLock<Arc<ReaderView>>` 短读锁 → 拷贝视图 → 无锁扫描 | 见 §3 末尾 |
| 快照一致性 | 读锁内取 `(seqno 水位, dead 位图, versions 版本链, vectors)` 一次成型 | 同快照内多次查询结果全等 |
| 并行扫描 | `std::thread::scope`(无 rayon) | 作用域线程保证借用安全,零生命周期泄漏;线程数默认取 `std::thread::available_parallelism()` |

**不承诺**:跨快照的可重复读(拿新快照自然看见新写入);这是嵌入型库的合理语义,
Agent 框架层若需事务语义由调用方组织。

**公开类型的线程安全**:`Mneme`/`Namespace`/`SnapshotHandle` 均 `Send + Sync`,
内部 `Arc` 克隆廉价,可跨线程共享;逐类型保证见 [16 §5](16-api-reference.md)。

---

## 8. 层边界契约(L1 → 上层)

**向上冻结**(= 公开 API,见 [01 §6](01-overview.md)、[16](16-api-reference.md) 与本章 §2):
`Mneme / Namespace / Record / Hit / RecordRef<'_> / SearchBuilder / InsertOutcome / UpdateOutcome /
UpdatePatch / Expr / Dedup / ResultDedup / Retention / Scoring / Diversity / RelationKind / Edge /
Feedback / ConsolidationPolicy 语义`,
以及 `search / insert / insert_batch / update / update_by_rowid / supersede / get / get_by_rowid / get_many /
get_many_by_rowid / get_vector / exists / count / delete / delete_by_rowid / touch /
touch_by_rowid / feedback / relate / relate_with_options / unrelate / neighbors / predecessors / consolidate /
forget / retain / iter / iter_with / flush / close / namespace / list_namespaces / drop_namespace /
snapshot / as_of / backup_to / stats / check / compact_control` 的签名。

> **冻结的是签名,不是全部实现**:`relate`/`neighbors`/`supersede`/`consolidate`/`feedback`
> 等在 L1 即以内存实现提供可用的最小语义(关系边、有效时间、简单摘要);产品能力层
> [09](09-memory-model.md)/[10](10-scoring.md) 在其上补齐持久化、双时态与打分的完整语义,
> **不改变签名**。因此 L1 仍是可独立交付的产品,后续层只增强语义。

**向下(L0)**:只使用 [02 §9](02-l0-core.md) 契约内的类型与函数。

**向 L3 交接的内部接口**(L3 引入 HNSW 时落地,以便内存层无痛升级为索引层;
L1/L2 阶段直接在引擎内实现公开签名,公开 API 始终不变):

设计最初的"全量 `VectorStore` trait(`insert/update/delete/get/search/relate`…)"在
落地时收敛为**只替换向量检索**的窄接口:`L1/L2` 继续直接实现全部公开 API,仅把
"暴力扫描"这一处抽成可替换的向量索引:

```rust
// memory::index(内部):公开 API 不变,只替换检索实现。
pub(crate) trait VectorIndex: Send + Sync {
    fn node_count(&self) -> usize;
    fn max_level(&self) -> u8;
    fn entry(&self) -> (SlotId, u8);
    fn serialize(&self) -> Vec<u8>;
    fn search(&self, params: &IndexSearch<'_>) -> TopK<(RowId, SlotId)>;
}
pub(crate) trait IndexFactory: Send + Sync {
    fn build(&self, nodes: &[IndexNode], params: HnswParams, metric: Metric) -> Arc<dyn VectorIndex>;
    fn load(&self, bytes: &[u8], nodes: &[IndexNode], slot_of: &[SlotId], metric: Metric)
        -> Result<Arc<dyn VectorIndex>>;
}
```

`L1` 的 `ReaderView` 随快照携带 `Option<Arc<dyn VectorIndex>>`(与段/表快照原子一致),
`search` 在读视图无索引或行数低于 `brute_force_max_rows` 时走暴力,否则**索引前缀走
ANN + 未落盘尾部暴力 + `TopK` 归并**。工厂由组合根(`Builder`)注入,`HNSW` 实现见
`crate::index`(设计 [05](05-l3-hnsw.md));`persist` 只经该接口安装/校验索引,不认识
`crate::index` 具体类型,层方向保持 L3 → L1。该接口对象安全(无 RPITIT),可存于 `Arc<dyn>`。

## 本章小结

- **API 是最大的风险**:先在纯内存层把公开 API 打磨冻结,后续层只换实现。
- 写入/读取/生命周期/落盘四组 API 的语义细则,是后续所有层的**行为规约**。
- 内存表 = `RowId` 版本链 + `delta` 覆盖层 + `(NsId, key)` 索引;读路径无锁扫描。
- 暴力扫描**过滤先行** + 分块并行;过滤 AST 采用三值语义;两级去重(FNV-1a + top-1)。
- **本章不变量**:I15(批量原子)、I16(优雅关闭)、I24(更新原子可见);内部 `memory::index::{VectorIndex, IndexFactory}` 接口衔接 L3。

## 下一章

[04-l2-persist.md](04-l2-persist.md):把内存中的世界搬到磁盘上,并且保证断电不丢。
