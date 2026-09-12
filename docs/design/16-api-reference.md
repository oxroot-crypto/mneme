# 16 公开 API 与运维参考

> **本章目标**:把散落在各章的公开接口收拢成一份可直接查阅的参考——
> 完整 API 清单、配置总表、打开已有库的校验规则、错误与重试指南、线程安全保证、
> 嵌入模型/Agent 框架集成方式、备份恢复 runbook 与数据限额。
> **前置阅读**:[01 §6](01-overview.md)(API 速览)、[03 §2](03-l1-memory.md)(语义细则)。
> **本章你将学到**:调用一个记忆库所需的全部工程细节,以及出问题时怎么办。

本章是**参考手册**,语义的推导与设计理由在 01–08 各章;此处只给"怎么用、边界在哪"。

---

## 1. 完整公开 API 清单

签名在 L1 冻结([01 §6](01-overview.md))。`Mneme` 是库句柄,`Namespace` 是逻辑分区,
二者都通过内部 `Arc` 共享、可自由克隆并跨线程传递(见 §5)。

> **实现状态**:本参考按冻结签名描述目标语义。L4 起 `text`(BM25)、`Fusion`、
> `Expr::from_str`/`Display`/JSON 往返与 `filter!` 均已落地;正文逐项标注了规划 API
> (L6/L11/L12)与未落地能力。**已接线但未落地**的配置项(如 `quantization`/`compression`)
> 当前仅记录配置、不生效,并以 `stats()` 如实反映;其余未落地能力以结构化错误返回、
> 绝不静默降级(FC-MEM-ERR-002),见 §4 错误表。

### 1.1 构建与打开

```rust
impl Mneme {
    /// 打开一个已初始化的本地目录记忆库。等价于 builder().path(dir).build()。
    /// 维度与度量从 MANIFEST 读回,无需重复指定;新建请用 builder().dimension(d).path(dir).build()。
    /// L2 已落地:持久库正常打开(维度/度量从 MANIFEST 读回)。
    pub fn open(path: impl AsRef<Path>) -> Result<Mneme>;

    /// 纯内存库(易失),维度必填。等价于 builder().dimension(d).build()(不设 path)。
    pub fn in_memory(dimension: u32) -> Result<Mneme>;

    pub fn builder() -> Builder;
    pub fn namespace(&self, path: &str) -> Namespace;      // 路径式,见 07 §5
}

impl Builder {
    pub fn path(self, dir: impl AsRef<Path>) -> Self;       // 省略 = 纯内存
    pub fn dimension(self, d: u32) -> Self;                 // 新建必填;打开时校验
    pub fn metric(self, m: Metric) -> Self;                 // 默认 Cosine;打开时校验
    pub fn fsync(self, p: FsyncPolicy) -> Self;             // 默认 Batched(20ms)
    pub fn insert_mode(self, m: InsertMode) -> Self;        // 默认 Upsert
    pub fn dedup(self, d: Dedup) -> Self;                   // 默认 Off
    pub fn dedup_threshold(self, t: f32) -> Self;           // 默认 0.95;近似去重余弦阈值,`[0,1]` 内的有限值(越界建库即拒绝)
    pub fn quantization(self, f: VectorFormat) -> Self;     // 默认 F32;L6 未落地,当前仅记录配置、不生效(stats().quant.active 恒 F32);见 08
    pub fn hnsw(self, p: HnswParams) -> Self;               // 默认 M=16/M0=32/efc=200/ef=64
    pub fn compaction(self, p: CompactionPolicy) -> Self;   // 默认见 §2
    pub fn retention(self, r: Option<Retention>) -> Self;   // 后台自动遗忘;默认 None = 关闭(见 07 §3.4)
    pub fn retain_interval(self, d: Duration) -> Self;      // 开启自动遗忘时的周期,默认 半衰期/4
    pub fn access_flush_interval(self, d: Duration) -> Self; // 默认 30s
    pub fn compression(self, c: Compression) -> Self;       // 文本/元数据压缩,默认 None(见 11;配置已接线,压缩实现待 L11)
    pub fn encryption(self, e: Option<Encryption>) -> Self; // 静态加密,feature `encrypt`(见 11;L11 落地,当前无此 API)
    pub fn storage(self, s: Arc<dyn Storage>) -> Self;      // 存储后端,默认 FsStorage;WASM/边缘自定义(见 12 §3;L12 落地,当前无此 API,内部直接 std::fs)
    pub fn read_only(self, yes: bool) -> Self;              // 只读共享模式(见 12 §2;L2 已实现单进程只读,不创建锁/WAL)
    pub fn read_only_probe_interval(self, d: Duration) -> Self; // 只读实例探测新 MANIFEST 的周期,默认 1s(见 12 §2;L12 落地,当前无此 API)
    pub fn verify_on_open(self, yes: bool) -> Self;         // 打开时全量校验各段 payload CRC,默认 false(见 04 §4.3)
    pub fn fail_fast_on_corruption(self, yes: bool) -> Self; // 损坏段直接拒绝启动,默认 false = 隔离剔除(见 04 §7)
    pub fn relation_index(self, r: RelationIndex) -> Self;  // 关系反向索引,默认 Outgoing(见 09 §2.3)
    pub fn parallelism(self, n: usize) -> Self;             // 默认 0 = available_parallelism()
    pub fn tuning(self, t: Tuning) -> Self;                 // 进阶调参,默认见 §2
    pub fn limits(self, l: Limits) -> Self;                 // 数据限额,见 §8
    pub fn clock(self, c: Arc<dyn Clock>) -> Self;          // 测试注入;默认 SystemClock
    pub fn fsync_hook(self, hook: Arc<dyn FsyncHook>) -> Self; // 测试崩溃注入,见 04 §10.1
    pub fn observer(self, o: Arc<dyn Observer>) -> Self;    // 可选可观测钩子,默认无(见 12 §4;L12 落地,当前无此 API)
    pub fn build(self) -> Result<Mneme>;
}
```

> **公开类型补充**:`lib.rs` 另导出 L0 标识类型 `Key`/`NsId`/`RowId`/`SegmentId`/
> `SeqNo`/`SlotId`(定义见 [02 §1](02-l0-core.md)),以及测试崩溃注入钩子
> `FsyncHook` 与 `IoAction`(见 [04 §10.1](04-l2-persist.md));`SeqNo` 与 `SlotId`
> 均不作为公开 API 的参数/返回类型(`SeqNo` 水位经 `SnapshotStats.version` 的 `u64`
> 诊断值展示,`SlotId` 数值不对外消费)。

### 1.2 写入

```rust
impl Namespace {
    /// 单条写入。语义见 03 §2.1。
    pub fn insert(&self, rec: Record) -> Result<InsertOutcome>;

    /// 批量写入,单次原子:要么整批可见,要么整批不可见(不变量 I15)。
    /// 整批共用一次 fsync 组提交;任一条校验失败 → 整批拒绝,不产生部分写入。
    pub fn insert_batch(&self, recs: Vec<Record>) -> Result<Vec<InsertOutcome>>;  // 整批原子:校验失败或 Merge 回调产物超限 → 整批回滚零部分写入

    /// 显式删除。返回是否命中活记录。
    pub fn delete(&self, key: &str) -> Result<bool>;

    /// 按全局稳定 RowId 删除(无 key 的记录也适用)。返回是否命中活记录。
    pub fn delete_by_rowid(&self, id: RowId) -> Result<bool>;

    /// 保留 RowId 的局部更新(按 key 定位);不存在返回 `UpdateOutcome::NotFound`。语义见 03 §2.1。
    pub fn update(&self, key: &str, patch: UpdatePatch) -> Result<UpdateOutcome>;
    pub fn update_by_rowid(&self, id: RowId, patch: UpdatePatch) -> Result<UpdateOutcome>;
}

/// 一条待写入的记忆。字段私有,经链式 setter 构造;`insert` 时统一校验维度与有限性。
/// `metadata`/`provenance` 缺省存 `null`;读出的 `Hit.metadata` / `RecordRef.metadata` 恒为
/// `Meta`(无元数据时即 `null`),`provenance` 保持 `Option`。
pub struct Record {
    vector: Vec<f32>,             // 必填;维度在 insert 时校验
    key: Option<String>,
    text: Option<String>,
    metadata: Option<Meta>,
    ttl: Option<Duration>,        // None = 永不过期
    importance: Option<f32>,      // None = 默认 0.5
    valid_from: Option<i64>,      // None = created_at(见 09 §3)
    valid_to: Option<i64>,
    confidence: Option<f32>,      // None = 默认 1.0
    provenance: Option<Meta>,
}

impl Record {
    pub fn new(vector: Vec<f32>) -> Self;                   // 维度在 insert 时校验,故无 Result
    pub fn key(self, k: impl Into<String>) -> Self;         // 可选外部键
    pub fn text(self, t: impl Into<String>) -> Self;        // 可选;启用 BM25/文本去重
    pub fn metadata(self, m: Meta) -> Self;                 // 可选 JSON
    pub fn ttl(self, d: Duration) -> Self;                  // 可选;默认永不过期
    pub fn importance(self, v: f32) -> Self;                // 可选;默认 0.5,越界钳制
    pub fn valid_from(self, ts_ms: i64) -> Self;            // 可选;有效时间起,缺省 = created_at(见 09 §3)
    pub fn valid_to(self, ts_ms: i64) -> Self;              // 可选;有效时间止(开区间)
    pub fn confidence(self, v: f32) -> Self;                // 可选;默认 1.0,越界钳制到 [0,1],参与打分(见 10 §2)
    pub fn provenance(self, p: Meta) -> Self;               // 可选;来源/派生链,设置后 RecordRef.provenance = Some(见 09 §4)
}

/// 局部更新补丁;`None` = 不改动该字段。字段语义见 [03 §2.1](03-l1-memory.md)。
pub struct UpdatePatch {
    pub vector: Option<Vec<f32>>,
    pub text: Option<Option<String>>,        // Some(None) = 清空
    pub metadata: Option<Option<Meta>>,      // Some(None) = 清空;Some(Some(v)) = 整体替换
    pub importance: Option<f32>,
    pub ttl: Option<Option<Duration>>,       // Some(None) = 取消过期
    pub valid_time: Option<(i64, Option<i64>)>,
    pub confidence: Option<f32>,
    pub provenance: Option<Option<Meta>>,    // Some(None) = 清空;见 09 §4
}
impl UpdatePatch { pub fn new() -> Self; /* 各字段的链式 setter */ }

/// 更新结果。
pub enum UpdateOutcome { Updated(RowId), NotFound }

/// 写入结果。`Inserted` = 新建;`Merged` = `Dedup::Merge` 就地更新并保留旧 RowId;
/// `Duplicate` = `Dedup::Reject` 去重拒绝时返回。`existing` 用 RowId 而非 Key,
/// 因为近似去重命中的记录可能没有 key(Key 可由 `get_by_rowid` 取回)。
pub enum InsertOutcome {
    Inserted(RowId),
    Merged(RowId),
    Duplicate { existing: RowId, score: f32 },
}

/// 检索命中的物化视图(只读)。**只用于带查询的 `search()`**。
/// 未开启 `Scoring` 时,`score` 是 `Metric::score` 的原始值(Dot/Cosine 越大越相似,
/// Euclidean 为距离平方、越小越近),命中列表按 `Metric::better` 排序(最优在前);
/// 开启 `Scoring` 后 `score` 为综合分(越大越优),按综合分降序。同分一律按 `rowid` 升序。
/// 注意:为控制内存,`Hit` 不携带原始向量;需要向量时用 `get_vector(rowid)`。
pub struct Hit {
    pub rowid: RowId,              // 全局稳定逻辑标识(见 02 §1);更新不改变
    pub query_id: QueryId,         // 本次查询的幂等标识,原样回传给 feedback(见 10 §4)
    pub key: Option<Key>,          // 写入时未给 key 则为 None
    pub score: f32,                // 最终分(默认相似度;开启 Scoring 后为综合分,见 10)
    pub created_at: i64,           // Unix 毫秒(事务时间)
    pub expires_at: Option<i64>,   // None = 永不过期
    pub importance: f32,
    pub confidence: f32,           // 可信度,见 09 §4
    pub valid_from: i64,           // 有效时间起(双时态,见 09 §3)
    pub valid_to: Option<i64>,     // 有效时间止
    pub text: Option<String>,
    pub metadata: Meta,
    pub provenance: Option<Meta>,  // 来源/派生链,见 09 §4(与 RecordRef 保持一致)
    pub via: Option<Edge>,         // 由关系扩展命中时的来源边(见 10 §3),否则 None
}
impl Hit {
    /// 各打分因子贡献(相似度/新鲜度/重要度/访问/可信度),用于调试与审计(见 10 §2.3)。
    pub fn explain(&self) -> ScoreBreakdown;
}
pub struct ScoreBreakdown { pub sim: f32, pub recency: f32, pub importance: f32, pub access: f32, pub confidence: f32, pub boost: f32 }

/// 存储记录的只读视图,**用于 `get` / `iter` / `get_many`(没有查询,故没有 `score`)**。
/// 内部以 `Arc` 持有物理版本,故 `get() -> RecordRef<'_>` 在安全 Rust 下成立;
/// `key()`/`text()`/`vector()` 访问器零拷贝(仅 `Arc` 引用计数),`metadata()` 借用
/// 已解析的 JSON。需要高吞吐只取向量时用 `get_vector`。详见 [03 §2.2](03-l1-memory.md)。
pub struct RecordRef<'a> {
    /* private: Arc<SlotData> + PhantomData<&'a ()> */
}
impl<'a> RecordRef<'a> {
    pub fn rowid(&self) -> RowId;
    pub fn key(&self) -> Option<&str>;
    pub fn created_at(&self) -> i64;
    pub fn expires_at(&self) -> Option<i64>;
    pub fn importance(&self) -> f32;
    pub fn text(&self) -> Option<&str>;
    pub fn metadata(&self) -> &Meta;
    pub fn valid_from(&self) -> i64;
    pub fn valid_to(&self) -> Option<i64>;
    pub fn confidence(&self) -> f32;
    pub fn provenance(&self) -> Option<&Meta>;
    pub fn vector(&self) -> &[f32];      // 零拷贝
    pub fn to_record(&self) -> Record;   // 克隆为可写 Record(去重 Merge 回调等用)
}

```

### 1.3 读取

```rust
impl Namespace {
    pub fn search(&self) -> SearchBuilder;                  // 见下
    pub fn get(&self, key: &str) -> Result<Option<RecordRef<'_>>>;    // 快照一致的单点读
    pub fn get_by_rowid(&self, id: RowId) -> Result<Option<RecordRef<'_>>>;

    /// 批量点读:一次持读锁、批量定位;返回顺序与输入一一对应(缺失为 None)。
    pub fn get_many(&self, keys: &[&str]) -> Result<Vec<Option<RecordRef<'_>>>>;
    pub fn get_many_by_rowid(&self, ids: &[RowId]) -> Result<Vec<Option<RecordRef<'_>>>>;

    /// 存在性判定,不物化记录。
    pub fn exists(&self, key: &str) -> Result<bool>;

    /// 单独取回原始向量(不物化整条记录;批量场景比逐条 get 更省内存)。
    pub fn get_vector(&self, id: RowId) -> Result<Option<Vec<f32>>>;

    /// 统计命中行数(不物化记录,复用过滤器/索引)。
    pub fn count(&self, filter: Option<Expr>) -> Result<u64>;

    /// 按过滤条件遍历(导出/审计/重建用),快照一致;不参与 ANN。
    /// 物化命中行的 `Arc` 句柄(不复制记录体,$O(N_c)$ 空间,FC-MEM-CPLX-005)。
    /// `include_deleted=true` 时包含墓碑/已逻辑过期记录(仅供审计,见 07 §3.4)。
    /// 外层 `Result` 是"建立遍历"的错误;内层 `Result` 是逐行读取(段损坏/被删)的错误——
    /// 迭代中途的 I/O 失败必须能被调用方看见,绝不静默截断(I2)。
    pub fn iter(
        &self,
        filter: Option<Expr>,
    ) -> Result<impl Iterator<Item = Result<RecordRef<'_>>> + '_>;
    pub fn iter_with(
        &self,
        filter: Option<Expr>,
        include_deleted: bool,
    ) -> Result<impl Iterator<Item = Result<RecordRef<'_>>> + '_>;
}

impl SearchBuilder<'_> {
    pub fn vector(self, q: &[f32]) -> Self;                 // 向量通道;长度 ≠ 建库维度时 execute() 返回 DimensionMismatch
    pub fn text(self, q: &str) -> Self;                     // BM25 通道
    pub fn top_k(self, k: usize) -> Self;                   // 默认 10,上限 4096
    pub fn ef(self, ef: usize) -> Self;                     // 仅 L3+ 生效;上限 4096
    pub fn filter(self, e: Expr) -> Self;                   // 预过滤(语义见 03 §2.2)
    pub fn dedup(self, d: ResultDedup) -> Self;             // 结果级去重(见 06 §6)
    pub fn fusion(self, f: Fusion) -> Self;                 // 双通道融合(见 06 §4);未同时启用双通道时 execute() 返回 Config
    pub fn score(self, s: Scoring) -> Self;                 // 时序/重要度/访问感知打分(见 10 §2)
    pub fn diversify(self, d: Diversity) -> Self;           // MMR 多样性(见 10 §5)
    pub fn expand(self, e: RelationExpand) -> Self;         // 关系联想扩展(见 10 §3)
    pub fn as_of(self, ts_ms: i64) -> Self;                 // 双时态历史读(见 09 §3)
    pub fn query_id(self, id: QueryId) -> Self;             // 指定本次查询的幂等标识;默认由 execute() 生成(见 10 §4)
    pub fn rerank(self, r: Arc<dyn Reranker>) -> Self;      // 可选精排钩子
    pub fn execute(&self) -> Result<Vec<Hit>>;              // 至少一个通道非空,否则 Config;Fusion 未同时启用双通道或 Weighted.alpha 越界/非有限 → Config;查询向量维度不符返回 DimensionMismatch;MMR lambda 非有限值返回 Config
}
```

### 1.4 生命周期

```rust
impl Namespace {
    pub fn touch(&self, key: &str, boost: Option<f32>) -> Result<bool>;  // 访问计数 +1;boost=Some(d) 时 importance += d
    pub fn touch_by_rowid(&self, id: RowId, boost: Option<f32>) -> Result<bool>;
    pub fn feedback(&self, id: RowId, fb: Feedback, query_id: QueryId) -> Result<bool>;  // 检索反馈闭环,幂等键 (id, query_id);记录不可见(不存在/墓碑/过期)→ false 且不占幂等键(见 10 §4)
    pub fn forget(&self, filter: Expr) -> Result<usize>;    // 主动遗忘,返回删除数
    pub fn retain(&self, policy: Retention) -> Result<RetainReport>;

    // ---- 双时态(见 09 §3)----
    /// 信念修订:更新同 key,并把旧版本 `valid_to` 闭合为新版本 `valid_from`。
    /// 要求该 key 已存在(不存在返回 `UpdateOutcome::NotFound`);首个版本请用 `insert`/upsert。
    /// 新记录沿用目标 key:省略 `rec.key` 时继承 `key`;显式给出且冲突 → `KeyMismatch`。
    pub fn supersede(&self, key: &str, rec: Record) -> Result<UpdateOutcome>;

    // ---- 记忆关系(见 09 §2)----
    pub fn relate(&self, from: RowId, to: RowId, kind: RelationKind, weight: f32) -> Result<()>;
    /// 同 `relate`,经 `RelateOptions` 打包 kind/weight/metadata(幂等键仍为 `(from, to, kind)`)。
    pub fn relate_with_options(&self, from: RowId, to: RowId, options: RelateOptions) -> Result<()>;
    pub fn unrelate(&self, from: RowId, to: RowId, kind: RelationKind) -> Result<bool>;
    pub fn neighbors(&self, from: RowId, kinds: &[RelationKind]) -> Result<Vec<Edge>>;
    /// 入边:返回所有 `to == to` 且 `kind ∈ kinds` 的边;默认全段扫描,
    /// `RelationIndex::Both` 时走反向索引(见 [09 §2.3](09-memory-model.md))。
    pub fn predecessors(&self, to: RowId, kinds: &[RelationKind]) -> Result<Vec<Edge>>;

    // ---- 记忆沉淀(见 09 §5)----
    pub fn consolidate(&self, policy: ConsolidationPolicy) -> Result<ConsolidateReport>;
}
```

### 1.5 命名空间

```rust
impl Mneme {
    pub fn namespace(&self, path: &str) -> Namespace;               // 路径规范化(去首尾 `/`、合并 `//`);首次写入时惰性登记并校验深度/字符(见 07 §5)
    pub fn list_namespaces(&self) -> Result<Vec<String>>;           // 规范化路径字典序
    pub fn drop_namespace(&self, path: &str) -> Result<usize>;      // 按 `/` 段边界级联墓碑,返回行数;注销经 WAL 持久化(FC-LIFE-POST-006)
}
```

### 1.6 快照与运维

> 本节汇总快照句柄、策略类型、运维报告与运行统计的完整定义。

```rust
impl Mneme {
    pub fn snapshot(&self) -> SnapshotHandle;   // 钉住当前 ReaderView(含可变表快照)
    pub fn as_of(&self, ts_ms: i64) -> Result<SnapshotHandle>;  // 双时态历史读:版本链上取 tx_ms ≤ ts 的可见版本(默认永久保留,见 07 §4.2a)
    pub fn backup_to(&self, dir: impl AsRef<Path>) -> Result<BackupReport>;  // 先 flush 再备份(见 §7);L2 已落地(纯内存库返回 `Unsupported`)
    pub fn stats(&self) -> Result<Stats>;
    pub fn check(&self) -> Result<CheckReport>;  // fsck:key 索引 ↔ 最新版本对账 + 段 CRC/版本链 + 死比率与合并建议(FC-LIFE-POST-009)
    pub fn compact_control(&self) -> CompactionControl;  // pause()/resume()/state()
    pub fn compact(&self) -> Result<()>;         // 显式触发一轮 size-tiered compaction(无触发/暂停/纯内存库时空操作,见 07 §4)
    pub fn maintenance_tick(&self) -> Result<()>; // 手动执行一轮后台维护(访问攒批/自动遗忘/自动 compaction,见 07 §2–§4)
    pub fn flush(&self) -> Result<()>;           // 把可变表落成增量段并 fsync WAL
    pub fn close(self) -> Result<()>;            // flush + 停后台维护 + 释放文件锁;幂等(对已关闭的库经其他句柄再调返回 Ok)
}

impl SnapshotHandle {
    pub fn version(&self) -> u64;                          // 构建该视图时的基线序号水位(view.seqno)
    pub fn as_of_ms(&self) -> i64;                         // 事务时间上界;普通 snapshot() = 当前,as_of(t) = t
    /// 在钉住的快照上取命名空间视图(键唯一性按命名空间隔离,故读取须先选命名空间)。
    pub fn namespace(&self, path: &str) -> SnapshotNamespace;
    pub fn stats(&self) -> SnapshotStats;                  // 快照钉住的视图统计(不随后续写入/compaction 变化)
}

/// 快照上的命名空间只读视图;持有快照视图的 `Arc`,克隆廉价、可跨线程使用。
/// 读取面与 `Namespace` 对齐(不含任何写方法);`include_deleted` 语义同 `Namespace::iter_with`。
impl SnapshotNamespace {
    pub fn search(&self) -> SearchBuilder;                 // 在钉住的视图上查询
    pub fn get(&self, key: &str) -> Result<Option<RecordRef<'_>>>;
    pub fn get_by_rowid(&self, id: RowId) -> Result<Option<RecordRef<'_>>>;
    pub fn get_many(&self, keys: &[&str]) -> Result<Vec<Option<RecordRef<'_>>>>;
    pub fn get_many_by_rowid(&self, ids: &[RowId]) -> Result<Vec<Option<RecordRef<'_>>>>;
    pub fn get_vector(&self, id: RowId) -> Result<Option<Vec<f32>>>;
    pub fn exists(&self, key: &str) -> Result<bool>;
    pub fn count(&self, filter: Option<Expr>) -> Result<u64>;
    pub fn neighbors(&self, from: RowId, kinds: &[RelationKind]) -> Result<Vec<Edge>>;
    pub fn predecessors(&self, to: RowId, kinds: &[RelationKind]) -> Result<Vec<Edge>>;
    pub fn iter(&self, filter: Option<Expr>)
        -> Result<impl Iterator<Item = Result<RecordRef<'_>>> + '_>;
    pub fn iter_with(&self, filter: Option<Expr>, include_deleted: bool)
        -> Result<impl Iterator<Item = Result<RecordRef<'_>>> + '_>;
}
```

> **`db.as_of(t)` 与 `search().as_of(t)` 的区别**:前者返回**历史快照句柄**(可反复
> `get`/`iter`/`search`,适合"回看某时刻我知道什么");后者是**单次检索**在版本链上按事务时间
> 取可见版本,适合"用旧知识做一次检索"。两者语义一致、粒度不同;历史默认永久保留
> (`history_horizon=None`,[07 §4.2a](07-l5-life.md))。

#### 策略与报告类型

```rust
/// 遗忘策略(写入期/后台共用)。`Retention::new()` 默认 half_life=14d、min_importance=0.2、access_weight=0.05。
pub struct Retention {
    pub half_life: Duration,
    pub min_importance: f32,
    pub access_weight: f32,             // 访问增益权重(公式见 07 §3.3),默认 0.05;须为有限值,否则 retain 返回 Config
    pub protect: Option<Expr>,          // 白名单:命中者豁免
}
impl Retention {
    pub fn new() -> Self;
    pub fn half_life(self, d: Duration) -> Self;
    pub fn min_importance(self, v: f32) -> Self;
    pub fn access_weight(self, v: f32) -> Self;
    pub fn protect(self, e: Expr) -> Self;
}

/// 融合器。`Rrf` 默认 k=60(即 `Fusion::default()`);`Weighted { alpha }` 无 `Default`,
/// 设计推荐 0.5,须显式构造(未指定时默认融合为 `Rrf { k: 60 }`,见 06 §4)。
pub enum Fusion { Rrf { k: u32 }, Weighted { alpha: f32 } }

/// 运维报告类型(字段为稳定契约)。
pub struct RetainReport { pub scanned: usize, pub forgotten: usize, pub sampled_ids: Vec<RowId> }  // 可审计(I23)
pub struct BackupReport { pub files: usize, pub bytes: u64, pub hardlinked: bool }
pub struct CheckReport  { pub ok: bool, pub corrupted: Vec<SegmentId>, pub suggestions: Vec<String> }
pub struct SnapshotStats { pub version: u64, pub segments: usize, pub rows: u64 }  // version = 视图基线序号水位;rows 为物理槽位数(含历史/墓碑)
```

#### 运行统计

```rust
/// `db.stats()` 的运行统计(字段为稳定契约)。
pub struct Stats {
    pub segments: Vec<SegmentStat>,
    pub wal_bytes: u64,
    pub memory_est: u64,                          // 进程内存估算(不含 OS 页缓存)
    pub trash_bytes: u64,
    pub query_latency: Histogram,                 // 固定 32 桶(对数刻度,1ms..1s)
    pub per_namespace: HashMap<String, NsStat>,   // 键为命名空间路径(经 MANIFEST 注册表解析)
    pub quant: QuantStat,                         // 量化状态与召回估计(08 §4.3、I13)
    pub compaction: CompactionState,
    pub retain: Option<RetainReport>,             // 最近一次后台遗忘(未开启则 None,见 07 §3.4)
    pub relations: u64,                           // 关系边数(见 09 §2)
    pub history: HistoryStat,                     // 版本链/历史保留统计(见 07 §4.2a)
    pub storage: StorageStat,                     // 存储安全配置与迁移进度(压缩实现待 L11;见 11)
}
// `SegmentStat` 标注 #[non_exhaustive](字段随层扩展,下游不得穷尽构造/匹配);
// `bytes` = 段内 vsec + 已落盘 hidx 字节数(不含 msec);`rows` 含墓碑与历史版本。
pub struct SegmentStat { pub id: SegmentId, pub rows: u64, pub bytes: u64, pub dead_ratio: f32, pub created: i64, pub index_nodes: u64, pub index_levels: u8 }  // index_* 为 L3 HNSW 图统计(无索引段为 0;小段可能预建但查询恒暴力)
pub struct NsStat      { pub doc_count: u64, pub total_doc_len: u64 }
pub struct Histogram   { /* 固定 32 桶边界与计数,详见 07 §7 */ }
pub struct StorageStat { pub encryption: bool, pub compression: Compression, pub migrated_segments: usize, pub total_segments: usize }
```

> **API 变更记录(L3,破坏性)**:`SegmentStat` 新增 `index_nodes`/`index_levels` 两个公开字段,
> 并标注 `#[non_exhaustive]`(统计字段随层扩展;外部可读但不得再以结构体字面量构造或穷尽匹配)。
> 兼容口径:v0.1.0 未发布,下游无既有构造点;后续新增字段不再构成破坏性变更。正式 RFC
> 流程(`CONTRIBUTING.md` §3)落地前,以本记录作为变更登记。

```rust
/// 版本链/历史保留统计(见 [07 §4.2a](07-l5-life.md))。
pub struct HistoryStat {
    pub retained_versions: u64,     // 当前保留的物理版本总数(含历史版本与墓碑)
    pub reclaimed_versions: u64,    // 累计已回收的版本数(仅 history_horizon 有限时增长)
    pub horizon: Option<Duration>,  // 当前生效的历史保留窗口,None = 永久
}

/// 量化运行状态:`configured` 为用户配置,`active` 为实际生效格式(可能因 I13 自动回退)。
/// **L6 未落地**:量化副本与两阶段检索尚无实现,`active` 当前恒为 `F32`,绝不回显配置;
/// `recall_est` 为抽样查询的粗/精排名一致率估计(见 [08 §4.3](08-l6-quant.md))。
pub struct QuantStat { pub configured: VectorFormat, pub active: VectorFormat, pub recall_est: Option<f32> }

/// 后台合并状态(`stats()` 的 `compaction` 字段);运行中 `pause()` 转 `Paused`。
pub enum CompactionState {
    Idle,
    Running { progress: f32, segments: Vec<SegmentId> },
    Paused { progress: f32, segments: Vec<SegmentId> },
}
impl CompactionControl {
    pub fn pause(&self);
    pub fn resume(&self);
    pub fn state(&self) -> CompactionState;
}

/// 单条记录的访问统计(内存累积,见 [07 §2](07-l5-life.md))。
pub struct AccessStat { pub last_access_ms: i64, pub access_count: u32 }
```

`SnapshotHandle` 在自身存活期间看到**完全一致的过去**:即使后台 compaction 推进,
它引用的段文件因 `Arc` 引用而不会被物理删除([07 §6](07-l5-life.md)、不变量 I17)。

### 1.7 关闭与 `Drop`

- **推荐显式 `db.close()`**:它执行 `flush` 后释放文件锁,返回 `Ok` 即代表
  所有已确认写入已持久(不变量 I16)。
- `Drop` **不做 flush**:只停止后台维护线程并释放句柄(锁随 `Store` 释放),
  无法向调用方报告结果;需要确保持久性时不要依赖析构(已确认写入靠 WAL 恢复,见 §1.7 上条)。
- **克隆与关闭语义**:`Mneme` 经内部 `Arc` 克隆,`close(self)` 关闭的是**共享库**;
  首个 `close` 完成 `flush` 并释放文件锁,此后其余克隆(及其 `Namespace`)上的
  操作返回 `Closed`,重复 `close` 返回 `Ok`。多线程长期共享时,应在确认
  所有使用方结束后再 `close`(见 [03 §2.4](03-l1-memory.md))。
- 进程被 `abort`/断电时,已 fsync 的写入仍由 WAL 恢复([04 §7](04-l2-persist.md))。

### 1.8 过滤、去重与重排类型

```rust
/// 过滤 AST(定义见 [03 §5.1](03-l1-memory.md))。
pub enum CmpOp { Eq, Ne, Gt, Ge, Lt, Le }
pub enum Val { Bool(bool), Int(i64), Num(f64), Str(Arc<str>), Ts(i64) }
pub enum Expr {
    Cmp { op: CmpOp, field: String, val: Val },
    In(String, Box<[Val]>),
    Contains(String, Val),                    // 数组含元素 / 字符串含子串
    StartsWith(String, Arc<str>), EndsWith(String, Arc<str>),
    Glob(String, Arc<str>),                   // 通配符:* 任意串,? 单字符
    Exists(String), IsNull(String),
    And(Box<[Expr]>), Or(Box<[Expr]>), Not(Box<Expr>),
    Always, Never,
}
impl Expr {
    /// 组合器入口:`Expr::field("importance").gt(0.5)`;`&` / `|` 运算符重载见 03 §5.1。
    pub fn field(name: &str) -> FieldBuilder;
    /// DSL 字符串解析,对任意输入不 panic(I7);`filter!` 即其 expect 封装(见 06 §1)。
    pub fn from_str(s: &str) -> Result<Expr>;
    /// 编码为 JSON(单键对象,见 06 §1)。
    pub fn to_meta(&self) -> Meta;
    /// 从 JSON 解码;结构非法返回 `FilterParse`。
    pub fn from_meta(meta: &Meta) -> Result<Expr>;
}
// `Display`(打印为可被 `from_str` 读回的文本)亦已实现,见 06 §1。

/// 分词:按空白切词 + CJK bigram + 可选停用词;公开给自建文本索引的宿主(见 06 §3.5)。
pub fn tokenize(text: &str, stopwords_enabled: bool) -> Vec<String>;

/// 字段组合器(定义见 [03 §5.1](03-l1-memory.md)):`eq/ne/gt/ge/lt/le/is_in` 返回 `Expr`。
pub struct FieldBuilder { /* field: String */ }

// `filter!` 宏(库根导出):对**写死在代码里的**字面量做运行时解析,失败即 panic
// (文档化例外);等价于 `Expr::from_str(...).expect("invalid filter literal")`。
// 处理运行时输入请用 `Expr::from_str`(返回 `Result`,不 panic,I7);
// 展开形式等价于 `macro_rules! filter { (s:expr) => { Expr::from_str(s).expect(...) } }`。

/// 写入期去重策略(见 [03 §6.3](03-l1-memory.md));`Merge` 回调签名:
/// `fn(&RecordRef<'_>, &RecordRef<'_>) -> Option<Record>`,返回 None = KeepBoth。
/// 注意:回调是函数指针,**不能捕获宿主状态**;需要携带状态时请用非捕获函数 +
/// 全局/线程局部配置,或把状态编码进记录本身。
pub enum Dedup { Off, Reject, Replace, KeepBoth, Merge(fn(&RecordRef<'_>, &RecordRef<'_>) -> Option<Record>) }

/// 结果级去重,只作用于单次 `execute()` 的命中列表(见 [06 §6](06-l4-query.md))。
pub enum ResultDedup { Off, ById, Near { threshold: f32 } }

/// 精排钩子及其查询上下文(见 [06 §5](06-l4-query.md))。
pub struct QueryCtx<'a> { pub text: Option<&'a str>, pub vector: Option<&'a [f32]> }
pub trait Reranker: Send + Sync {
    fn rerank(&self, q: &QueryCtx, hits: Vec<Hit>) -> Vec<Hit>;
}

/// 综合排序打分(见 [10 §2](10-scoring.md))。各权重为 0 即关闭该因子。
pub struct Scoring {
    pub w_sim: f32,         // 相似度权重,默认 1.0
    pub w_recency: f32,     // 时间新鲜度权重,默认 0.0(关闭)
    pub w_importance: f32,  // 重要度权重,默认 0.0
    pub w_access: f32,      // 访问频次权重,默认 0.0
    pub w_confidence: f32,  // 可信度权重,默认 0.0
    pub half_life: Duration,// 新鲜度半衰期,默认 14d
    pub c_norm: u32,        // 访问次数归一基准,默认 100(见 10 §2.1)
    pub floor: f32,         // 相似度保底,防止低相似命中被时序因子顶上来,默认 0.0
    pub time_axis: TimeAxis,// 新鲜度时间轴,默认 ValidTime(见 10 §2.1)
    pub bias_routing: bool, // HNSW 遍历按重要性偏置(只改访问顺序),默认 false(见 10 §2.3)
}
impl Scoring { pub fn new() -> Self; }  // 字段公开,按需用结构体字面量覆盖
// `Scoring` 亦实现 `Default`:`Scoring::default()` 等价于 `Scoring::new()`
//(w_sim=1.0,其余权重 0),见 10 §2.5 与 FC-SCORE-POST-001。

/// 新鲜度打分使用的时间轴(见 [10 §2.1](10-scoring.md))。
pub enum TimeAxis { ValidTime, TransactionTime }

/// 结果多样性(见 [10 §5](10-scoring.md))。
pub enum Diversity { Off, Mmr { lambda: f32 } }   // lambda∈[0,1],越大越重相关性;设计推荐 0.7,须显式构造(`Diversity::default() == Off`);非有限值 execute() 返回 Config

/// 关系联想扩展(见 [10 §3](10-scoring.md)):hops 默认 1(最大 3),decay 默认 0.5/跳,
/// max_nodes 默认 4096(`visited` 与结果合计上限,封顶扩展延迟)。
pub struct RelationExpand { pub hops: u8, pub kinds: Vec<RelationKind>, pub decay: f32, pub max_nodes: usize }

/// 关系邻接索引方向(见 [09 §2.3](09-memory-model.md)):`Both` 额外建反向边索引(空间 ×2),
/// 供 `predecessors` 走 `O(log E + degree)`;`Outgoing` 下 `predecessors` 仍可用但走全段扫描。
pub enum RelationIndex { Outgoing, Both }

/// 关系类型:内置 + 用户自定义(见 [09 §2](09-memory-model.md))。
pub struct RelationKind(pub u16);
impl RelationKind {
    pub const DERIVED_FROM: Self; pub const SUPPORTS: Self;
    pub const CONTRADICTS: Self;  pub const RELATED: Self;
    pub fn custom(name: &str) -> Result<Self>;   // 名称→稳定编号(注册表见 09 §2;未落地,当前不提供)
}
/// 一条关系边。
pub struct Edge { pub from: RowId, pub to: RowId, pub kind: RelationKind, pub weight: f32, pub metadata: Meta }

/// `relate_with_options` 的参数结构(参数收敛,见 [09 §2.2](09-memory-model.md))。
pub struct RelateOptions {
    pub kind: RelationKind,             // 关系类型
    pub weight: f32,                    // 边权;写入时钳制到 [0,1]
    pub metadata: Meta,                 // 边元数据,默认 Meta::Null
}
impl RelateOptions {
    pub fn new(kind: RelationKind, weight: f32) -> Self;
    pub fn metadata(self, m: Meta) -> Self;
}

/// 检索反馈(见 [10 §4](10-scoring.md))。
pub enum Feedback { Used, Ignored, Corrected { by: RowId } }

/// 一次检索的幂等标识;`execute()` 生成并随 `Hit` 返回,反馈时原样回传(见 [10 §4](10-scoring.md))。
/// `execute()` 缺省生成的 `QueryId` 由进程级全局分配器分配(跨库实例共享编号空间,保证不冲突);
/// 调用方显式指定时须自行保证唯一性。
pub struct QueryId(pub u64);

/// 记忆沉淀策略与报告(见 [09 §5](09-memory-model.md))。
pub struct ConsolidationPolicy {
    pub filter: Option<Expr>,     // 候选范围,默认 None = 调用方命名空间全部活记录
    pub threshold: f32,           // 近似重复阈值,默认 0.95;[0,1] 内的有限值(越界入口拒绝)
    pub max_cluster: usize,       // 单簇上限,默认 32
    pub target: Option<String>,   // 摘要写入的命名空间路径,默认 None = 调用方所在命名空间
    pub summarizer: Option<Arc<dyn Summarizer>>,  // None = 引擎拼接
    pub keep_sources: bool,       // 是否保留来源(作为摘要的 DERIVED_FROM 边),默认 true
}
impl Default for ConsolidationPolicy { fn default() -> Self; }  // 各字段取上述默认值(见 01 §6 示例)
pub trait Summarizer: Send + Sync {
    fn summarize(&self, cluster: &[RecordRef<'_>]) -> Option<Record>;
}
pub struct ConsolidateReport {
    pub clusters: usize,      // 发生沉淀的簇数(单元素簇不计)
    pub merged: usize,        // 被合并的来源记录总数
    pub created: Vec<RowId>,  // 新生成的摘要记录 RowId
}
```

---

## 2. Builder 与配置总表

所有参数均为**默认值**,以 `Builder` 配置项暴露。

| 配置 | Builder 方法 | 默认 | 作用 / 详见 |
|---|---|---|---|
| 存储路径 | `.path` | 无(纯内存) | 目录 = 一个库 |
| 维度 | `.dimension` | 新建必填 | 1..=65536,建库后不可改 |
| 度量 | `.metric` | `Cosine` | `Cosine/Dot/Euclidean`,[02 §3](02-l0-core.md) |
| fsync 策略 | `.fsync` | `Batched(20ms)` | `Always/Batched/OnFlush/Never`,[04 §3](04-l2-persist.md) |
| 同 key 行为 | `.insert_mode` | `Upsert` | `Upsert/RejectDuplicate` |
| 去重策略 | `.dedup` | `Off` | `Off/Reject/Replace/KeepBoth/Merge`,[03 §6](03-l1-memory.md) |
| 去重阈值 | `.dedup_threshold` | `0.95` | 近似去重阈值,**统一按余弦相似度口径**(非余弦度量下引擎内部先归一化);`[0,1]` 内的有限值,越界建库即拒绝;与 `ResultDedup::Near` 独立 |
| 量化格式 | `.quantization` | `F32` | `F32/F16/I8Rescored`,[08](08-l6-quant.md);**L6 未落地**,当前仅记录配置、不生效(`active` 恒 `F32`) |
| HNSW 参数 | `.hnsw` | 见下 | `HnswParams` |
| compaction | `.compaction` | 见下 | `CompactionPolicy` |
| 历史保留窗口 | `.compaction(p)` 的 `p.history_horizon` | `None`(永久) | `Option<Duration>`;有限值可回收超期历史版本,见 [07 §4.2a](07-l5-life.md) |
| 自动遗忘 | `.retention` | **`None`(关闭)** | `Option<Retention>`;显式传入才开启后台 retain,见 [07 §3.4](07-l5-life.md) |
| 遗忘扫描周期 | `.retain_interval` | 半衰期/4 | 开启自动遗忘后的触发间隔 |
| 访问统计落盘 | `.access_flush_interval` | `30s` | 内存访问计数批量写 WAL 的周期,见 [07 §2](07-l5-life.md) |
| 压缩 | `.compression` | `None` | 文本/元数据压缩,见 [11 §3](11-security-storage.md);**L11 落地**,当前仅记录配置、不生效 |
| 加密 | `.encryption` | `None` | 静态加密(feature `encrypt`),见 [11 §2](11-security-storage.md);**L11 落地**,当前无此 API |
| 存储后端 | `.storage` | `FsStorage` | `Arc<dyn Storage>`;WASM/边缘自定义后端,见 [12 §3](12-deployment.md);**L12 落地**,当前无此 API |
| 只读共享 | `.read_only` | `false` | 多进程只读打开,见 [12 §2](12-deployment.md) |
| 只读探测周期 | `.read_only_probe_interval` | `1s` | 只读实例发现新 MANIFEST 的周期,见 [12 §2.1](12-deployment.md);**L12 落地**,当前无此 API |
| 关系索引 | `.relation_index` | `Outgoing` | `Outgoing/Both`;`Both` 空间 ×2,见 [09 §2.3](09-memory-model.md) |
| 并行度 | `.parallelism` | `0`(自动) | 并行扫描的线程数(当前仅用于暴力扫描分块);0 = `available_parallelism()` |
| 可观测 | `.observer` | 无 | 可选事件钩子,见 [12 §4](12-deployment.md);**L12 落地**,当前无此 API |
| 进阶调参 | `.tuning` | 见下 | `Tuning` |
| 数据限额 | `.limits` | 见 §8 | `Limits` |
| 启动全量校验 | `.verify_on_open` | `false` | `true` 时打开即校验各段 payload CRC(慢),见 [04 §4.3](04-l2-persist.md) |
| 损坏段 fail-fast | `.fail_fast_on_corruption` | `false` | `true` 时遇损坏段拒绝启动,而非隔离剔除,见 [04 §7](04-l2-persist.md) |
| 时钟 | `.clock` | `SystemClock` | 测试注入,见 [04 §10](04-l2-persist.md) |

```rust
pub struct HnswParams {
    pub m: u16,              // 默认 16  上层度数上限;建库校验 ≥ 2 且 ≤ 4096
    pub m0: u16,             // 默认 32  第 0 层度数上限;建库校验 ≥ m 且 ≤ 4096
    pub ef_construction: u16,// 默认 200 构建探查宽度;建库校验 ≥ 1
    pub ef_search: u16,      // 默认 64  查询探查宽度;建库校验 ∈ [1, Limits.ef_max]
}
// 违反上述域 → Config/LimitExceeded(FC-INDEX-PRE-001),绝不静默。
// 层级骰子系数 m_L = 1/ln(m) 为派生量,不单独暴露(见 05 §3.2、05 §11)。

pub struct CompactionPolicy {
    pub tier_ratio: u32,     // 默认 4   分级比 r
    pub tier_count: u32,     // 默认 4   同层合并阈值 T
    pub dead_ratio: f32,     // 默认 0.25 墓碑+过期占比触发线
    pub wal_bytes: u64,      // 默认 256MB WAL 压力触发线
    pub wal_file_bytes: u64, // 默认 64MB  单个 WAL 文件轮转阈值(已落地,见 04 §3.2、FC-PERSIST-POST-011)
    pub segment_rows: u64,   // 默认 8192  段初始目标行数 B(07 §4.2)
    pub io_budget: f32,      // 默认 0.30 后台合并磁盘配额
    pub history_horizon: Option<Duration>, // 历史版本保留窗口,默认 None = 永久(07 §4.2a)
}

/// 进阶调参(通常保持默认;调大以小段增多/内存为代价)。
pub struct Tuning {
    pub parallel_block: usize,          // 默认 8192  暴力扫描的计算分块粒度(03 §4.2)
    pub field_dict_max: u16,            // 默认 16    每段可索引字段上限(04 §5.1)
    pub bloom_fpp: f32,                 // 默认 0.01  布隆过滤器目标误判率(04 §5.3)
    pub brute_force_max_rows: u32,      // 默认 2048  段行数不超过此值恒用暴力(严格 > 才走图;05 §9)
    pub filter_post_threshold: f32,     // 默认 0.10  过滤三档:后过滤/放大后过滤分界(05 §8)
    pub filter_brute_threshold: f32,    // 默认 0.001 过滤三档:放大后过滤/候选暴力分界(05 §8)
    pub stopwords: bool,                // 默认 true  启用内置停用词表(06 §3.5;建库即锁定,既存库以 MANIFEST 为准)
}
```

### 2.1 推荐预设

| 场景 | 建议配置 |
|---|---|
| 会话级临时记忆 | `Record::ttl` 短 + `FsyncPolicy::Batched(20ms)` + 默认 `retention`;命名空间按会话分 |
| 长期偏好/事实 | `importance` 显式设高 + `Retention::min_importance` 提高 + `Dedup::Replace` 或 `Merge` |
| 只读/分析副本 | `.fsync(Never)` 仅限测试;生产只读副本仍用 `Batched`,备份目录 `open` 后勿写 |
| 延迟敏感 | `.quantization(I8Rescored)`(**L6 目标;未落地前无效**) + `ef=64~128`;`.parallelism(0)` 交给运行时 |
| 内存受限 | 默认 mmap + i8;定期 `backup_to` 后重建更小的段 |

> 以上只是起点:所有旋钮都有默认值,先用默认跑通,再按 `stats()` 的延迟直方图与召回基准调参。

---

## 3. 打开已有库与校验

**新建**:目录不存在时,`.dimension()` 必填,`.metric()` 缺省为 `Cosine`。

**打开**:目录已存在时,维度与度量**以 MANIFEST 为准**:

```text
1. Mneme::open(dir) 或 builder().path(dir).build()
2. 若调用方显式设置了 dimension/metric,与 MANIFEST 比对:
     不一致 → DimensionMismatch / MetricMismatch,拒绝打开(不静默改写)
3. 未显式设置 → 直接采用 MANIFEST 中的值
4. 单写者独占:目录已被其他写实例打开 → Busy(文件锁,见 §4);
   只读实例(read_only(true))不争抢写锁,可多进程并发(见 12 §2)
```

**空目录 / 半初始化目录**:目录存在但没有 `current`/`MANIFEST.*` 时,视为**新建**
(`.dimension()` 必填);若只有 `MANIFEST.*` 而无 `current`(初始化在写 current 前崩溃),
按 [04 §6](04-l2-persist.md) 扫描合法 MANIFEST,全无则同样按新建处理。
若目录里有无法识别的残留文件但不含 `current`/`MANIFEST.*`,仍按新建;含 `current`
但 MANIFEST 全坏则报 `Corrupted`,绝不覆盖已有数据。

> **设计理由**:记忆库的维度是数据的一部分,不能由调用方每次"猜"。
> 让 MANIFEST 成为唯一事实来源,避免"同目录两种维度"导致的静默错读。
>
> **文件锁实现**:写实例打开/新建库目录下常驻的 `LOCK` 文件,并对其调用
> `std::fs::File::try_lock` 获取整文件 **OS 咨询锁**(Unix `flock`、Windows `LockFileEx`,
> 由 `std` 封装,Rust 1.89 起稳定,本库 MSRV 1.93 满足)。第二个**写**实例 `try_lock` 失败(WouldBlock)即返回
> `Busy`;**进程崩溃/退出时内核自动释放锁**,后续实例直接获取,无需租约刷新、心跳线程或
> 陈旧锁接管,也绝不删除锁文件(删除会使不同 inode 各自加锁而破坏互斥,见 [04 §13](04-l2-persist.md))。
> `close()` 释放句柄即解锁;锁文件本身保留。**只读实例不创建/不持锁,也不探测写者状态**
> ([12 §2](12-deployment.md))。

维度、度量之外的**可变配置**(fsync、去重、量化、HNSW、compaction、limits)
每次打开时可自由调整,不构成不兼容。

---

## 4. 错误处理与重试

统一错误类型见 [02 §2](02-l0-core.md)。按"能否重试"分类:

| 错误 | 可重试? | 处置建议 |
|---|---|---|
| `Io` | ✅ 通常可 | 瞬时 I/O 抖动;指数退避重试。`std::io::ErrorKind::NotFound`/`Other` 需先排查环境(注意:这是 `std::io::ErrorKind`,与引擎的 `ErrorKind` 不同) |
| `Busy` | ✅ 可 | 另一实例持有文件锁或正在备份;退避后重试,或确保单进程独占 |
| `DuplicateKey` | ✅ 可 | 改用 `InsertMode::Upsert`,或先 `get` 再决定 |
| `KeyNotFound` | ❌ | **当前无 API 产生**(保留变体);`get` 返回 `Option`、`delete`/`touch` 返回 `bool`,均不以缺失报错 |
| `DimensionMismatch` | ❌ | 调用方 bug:向量长度 ≠ 建库维度 |
| `MetricMismatch` | ❌ | 打开参数与库不一致;去掉显式 metric 或改对 |
| `KeyMismatch` | ❌ | `supersede` 新记录自带的 key 与目标 key 冲突;省略 key 以继承目标 key |
| `FilterParse` | ❌ | DSL 语法错误,错误携带位置;修正表达式 |
| `TooLarge` / `LimitExceeded` / `MetaTooDeep` | ❌ | 数据/参数超限,见 §8 |
| `NonFinite` | ❌ | 向量分量或 `importance`/`confidence`/边权/`boost` 含 `NaN`/`±Inf`,会污染排序与打分;修正输入 |
| `Closed` | ❌ | 库已关闭;不要再使用该库的任何克隆句柄 |
| `Config` | ❌ | 建库/查询配置非法(缺维度、无查询通道、MMR `lambda` 非有限值、`Fusion` 未同时启用双通道、`Weighted.alpha` 越界或非有限),策略参数含非有限值(`min_importance`/`access_weight`/`threshold`/`dedup_threshold`)或非法(如 `max_cluster = 0`) |
| `Unsupported` | ❌ | 该能力延后到后续层,或对当前形态不适用(**纯内存库 `backup_to`**;只读模式写);按版本升级 |
| `Inconsistent` | ❌ | 内部不变量被破坏(应为 bug);上报并附上下文 |
| `UnsupportedVersion` | ❌ | 文件格式版本与当前定义不一致(未发布期无旧格式兼容);从备份恢复或重建 |
| `Corrupted` | ❌ | 数据损坏:立即停止写入,跑 `db.check()`,按 §7 恢复 |

**原则**:错误信息面向排查——`Corrupted` 带段号与原因,`FilterParse` 带出错位置
([02 §2](02-l0-core.md))。库本身**绝不 panic**;唯三的例外是:① `async` 门面中
`spawn_blocking` 任务被取消/panic 的 `expect`([08 §6](08-l6-quant.md));
② `filter!` 宏对写死的非法字面量在展开处 panic——运行时输入请用 `Expr::from_str`
返回的 `Result`([06 §1.1](06-l4-query.md)、不变量 I7);
③ 候选收集/槽位映射处对槽位下标的 `u32::try_from(..).expect`(如并行扫描、
`query::plan::compile`、`WriterState::rebuild_indexes`,共六处),`commit_version` 经
`slot_id_for` 拒绝溢出(FC-MEM-INV-004),该转换可证明不会失败;另有 `persist::wal::codec`
的 `encode_frame` 负载长度转换(`FC-GLOBAL-PRE-003` 限额保证远小于 `u32::MAX`)一并
登记为同源文档化例外(FC-GLOBAL-ERR-001)。

---

## 5. 线程安全与 `Send + Sync`

| 类型 | `Send` | `Sync` | 说明 |
|---|---|---|---|
| `Mneme` / `Namespace` | ✅ | ✅ | 内部 `Arc`;克隆廉价,可跨线程共享 |
| `SnapshotHandle` | ✅ | ✅ | 只读视图,任意线程并发查询 |
| `SnapshotNamespace` | ✅ | ✅ | 快照上的命名空间只读视图,持有快照 `Arc` |
| `AsyncNamespace` | ✅ | ✅ | async 门面,共享同一底层句柄(feature `async`,**L6 规划,当前无此类型**) |
| `SearchBuilder` | ✅ | ✅ | 短生命周期构建器,通常单线程用完即 `execute`(仅不承诺跨线程可变使用) |
| `Record` / `Hit` / `InsertOutcome` / `UpdatePatch` / `QueryId` | ✅ | ✅ | 值类型 |
| `RecordRef<'_>` | ✅ | ✅ | 以 `Arc` 持有记录数据的只读视图 |
| `Reranker` / `Clock` / `Summarizer` / `KeyProvider` / `Observer` | ✅ | ✅ | 宿主实现需满足(`KeyProvider`/`Observer` 为 L11/L12 规划,当前无此类型) |
| `Storage` | ✅ | ✅ | 平台存储后端(见 12 §3,`trait` 为 L12 规划) |

**并发语义**:

- **单写者多读者**:写路径全局串行(一把写锁),读路径拿不可变视图后无锁扫描
  ([01 §2.1](01-overview.md)、[03 §7](03-l1-memory.md));
- 同一快照内任意并发查询结果**完全一致**(排序全等性,[03 §2.2](03-l1-memory.md));
- 不承诺跨快照可重复读:拿新快照自然看见新写入;
- `SnapshotHandle` 与前台写入互不阻塞。

---

## 6. 与嵌入模型 / Agent 框架集成

Mneme **只存向量**,不内置任何嵌入推理。宿主负责把文本变成向量:

```rust
// 1. 宿主调用外部嵌入模型(OpenAI / BGE / 本地模型……)
let vector: Vec<f32> = embedder.embed("用户偏好深色模式")?;

// 2. 交给 Mneme 存储;原文放 text 以便 BM25 与展示
ns.insert(
    Record::new(vector)
        .key(memory_id)
        .text("用户偏好深色模式")
        .metadata(json!({"kind": "preference"}))
        .importance(0.8)
)?;

// 3. 检索时同样先嵌入查询
let q = embedder.embed("他喜欢什么界面风格?")?;
let hits = ns.search().vector(&q).text("界面风格").top_k(10).execute()?;
```

**与编排框架(mem0 / Letta 等)的分工**:编排层决定"记什么、忘什么、何时调用",
Mneme 提供引擎级支撑——命名空间隔离、去重、TTL/遗忘、混合检索。
典型用法:每个会话一个子命名空间 `agent-42/session-88`,长期偏好放 `agent-42/profile`。

**重要约束**:同一库的所有向量必须来自**同一嵌入模型**且维度一致;换模型 =
新建库 + 重新灌入(不承诺跨模型向量混用)。

---

## 7. 备份与恢复 runbook

### 7.1 常规备份

```rust
db.backup_to("./backup")?;   // 一致性快照:先 flush,再复制/硬链接段、MANIFEST 与 WAL
```

**目标目录语义**:目标必须不存在或为空;若已含库内容则返回 `Busy`(不合并、不覆盖),
避免把两次备份混在一起。备份写入是"先写全部文件、最后写 `current`"的顺序,
中途失败会留下一个不含合法 `current` 的目录——它无法被打开,重新备份即可(不污染源库)。
`BackupReport.hardlinked` 标记是否走硬链接:L5 起同盘优先硬链接,失败/跨盘回退逐文件复制,
任一文件回退即置 `false`(见 [07 §6](07-l5-life.md))。

备份目录是一个**可独立打开**的完整库;验证:

```rust
let b = Mneme::open("./backup")?;
b.check()?;                  // 全绿 = 备份有效(写进 CI,见 14 §6)
```

### 7.2 时间点恢复(PITR)

**备份只含备份时刻的当前 MANIFEST**:`backup_to` 复制段文件、**当前 MANIFEST**、WAL 与
`current`,**不保留更早的 MANIFEST 版本**([04 §6](04-l2-persist.md))。因此单份备份**不能**
通过改 `current` 回滚到更早提交点;要支持 PITR,需按目标时间点**定期执行 `backup_to`**,
每份备份各自独立可打开。

实时库自身保留最近 2 个 MANIFEST 版本(write-once + 指针,`MANIFEST_KEEP = 2`,见
[04 §6](04-l2-persist.md));若最新 `MANIFEST.*` 损坏,打开时扫描目录自动回退到上一合法版本
([04 §7](04-l2-persist.md))。

### 7.3 损坏处置

```text
1. 立即停止写入(另起进程打开会拿到 Busy;已打开的实例先 close)
2. db.check() 定位损坏段(报告段号/CRC/对账差异)
3. 若仅个别段头损坏:恢复流程会将其移入 trash/ 并从视图剔除(04 §7),
   剩余数据仍可用;导出仍完好的数据:
       for rec in ns.iter(None)? { let rec = rec?; /* 写入新库 */ }
       (逐行 `Result` 可定位到具体损坏段,便于决定跳过还是中止)
4. 若 MANIFEST 全坏:扫描 MANIFEST.* 取最大合法版本(04 §6);
   仍失败 → 用最近备份 open + check,确认后作为新库
5. 恢复后立即 db.backup_to(...) 留档,并核对 stats() 行数
```

**磁盘满(ENOSPC)**:写入返回 `Io`;compaction 会暂停而非损坏数据。
清理 `trash/` 或扩容后重试 `flush()`;不要删除 `wal/` 或 `segments/` 下的文件。

---

## 8. 数据限额与校验

超限一律返回 `TooLarge` / `LimitExceeded` / `MetaTooDeep`,**绝不静默截断**。

| 项 | 默认上限 | 备注 |
|---|---|---|
| 维度 | 65536 | 新建时校验 |
| key 长度 | 1024 字节 | UTF-8 编码后 |
| text 长度 | 1 MiB | 超出拒绝;BM25 对超长文本收益低 |
| metadata JSON | 64 KiB | 超出拒绝 |
| metadata 嵌套深度 | 32 层 | 防解析栈溢出 |
| `top_k` | 4096 | [03 §2.2](03-l1-memory.md) |
| `ef` | 4096 | 仅 L3+ |
| WAL 单帧 payload | 16 MiB | 撕裂写检测与内存上界;**当前保留限额,尚未在写路径强制** |
| 命名空间深度 | 32 级 | `a/b/c/...`,对应 `Limits.ns_depth` |
| 自定义关系类型 | 65520 个 | u16 编号空间,内置占用 0..=15;超限 `TooLarge` |

限额通过 `.limits(Limits { .. })` 调整;调大以内存/恢复时间为代价,请评估后再改。

> **向量取值校验**:插入时逐个分量检查是否为有限值,`NaN`/`±Inf` 一律返回 `NonFinite`
> (否则会污染 `Metric::better` 的排序与 TopK)。**标量因子同口径**:`importance`/`confidence`
> (含 `UpdatePatch` 与 `touch` boost)、关系边权含非有限值时同样返回 `NonFinite`,绝不入库。
> 零向量合法:余弦按 [02 §3.3](02-l0-core.md)
> 返回 0,但语义上是"无方向",调用方自行判断是否需要拒绝。

### 8.1 过滤保留字段

过滤 DSL 的字段既可能是用户 metadata,也可能是引擎保留字段。保留字段**不在**
`Meta` JSON 里,而由引擎直接解析,且优先级高于同名 metadata:

| 字段 | 类型 | 说明 |
|---|---|---|
| `rowid` | 数值 | 全局稳定记录标识;可用于 `forget`/`iter` 精确定位无 key 记录 |
| `key` | 字符串 | 外部键;未设 key 的记录不参与等值匹配 |
| `created_at` | 时间(Ts) | 写入时间,恒定索引([04 §5.2](04-l2-persist.md)) |
| `expires_at` | 时间(Ts) | TTL 到期时刻;无 TTL 记录缺失 |
| `importance` | 数值 | 记录的重要性(默认 0.5),**不是** metadata 里的同名键 |
| `confidence` | 数值 | 可信度(默认 1.0),见 [09 §4](09-memory-model.md) |
| `valid_from` | 时间(Ts) | 有效时间起(双时态),见 [09 §3](09-memory-model.md) |
| `valid_to` | 时间(Ts) | 有效时间止;无则视为至今有效 |
| `last_access` | 时间(Ts) | 最近访问时刻 |
| `access_count` | 数值 | 累计访问次数 |
| `__ns` | 字符串 | 命名空间路径([07 §5](07-l5-life.md)) |

> **`importance` 的单一来源**:`Record::importance()` 是唯一权威值;示例里
> `metadata(json!({"importance": ...}))` 只是普通 metadata,过滤时会被保留字段遮蔽。
> 推荐始终用 `Record::importance()` 设置重要性,不要在 metadata 里重复。

---

## 9. 层边界契约(L6 → 外部)补充不变量

面向使用者的不变量:**I15、I16 由本章定义**;I17 见 [07 §8](07-l5-life.md),
I18 见 [04 §14](04-l2-persist.md)。为便于查阅,四条一并列出:

- **I15 批量原子**:`insert_batch` 要么整批可见、要么整批不可见,不存在部分写入;
- **I16 优雅关闭**:`close()` 返回 `Ok` 后,所有已确认写入持久;`Drop` 不保证;
- **I17 快照一致**:`SnapshotHandle` 存活期间看到固定 ReaderView 的完整视图(段集 + 取快照时的可变表快照),后台 compaction 不影响其可见性与正确性;
- **I18 版本精确匹配**:只接受 `format_version == FORMAT_VERSION` 的文件;
  任何版本不一致返回 `UnsupportedVersion`,拒绝打开而非静默误读([04 §12](04-l2-persist.md))。

其余不变量(I19–I30,完整定义见 [spec/contracts.md](../spec/contracts.md)):

- **I19 覆盖持久性**:`delete`/`update`/`touch`/`relate` 返回 `Ok` 后,崩溃 + WAL 截断仍生效,删除永不复活([04 §14](04-l2-persist.md));
- **I20 注册与水位可恢复**:`path↔NsId`、`next_ns_id`、`next_rowid` 可由 MANIFEST+WAL 完整重建,ID 永不复用([04 §3.3](04-l2-persist.md));
- **I21 BM25 统计一致性**:N/avgdl/df 按查询命名空间跨全部活跃段全局聚合、只计活行,与段数无关,跨 NS 互不影响([04 §5.6](04-l2-persist.md)、[06 §3.2](06-l4-query.md));
- **I22 稳定逻辑标识**:RowId 跨 `update`/upsert 不变,访问统计与关系边始终有效([02 §1](02-l0-core.md));
- **I23 删除可审计与安全默认**:自动遗忘默认关闭;删除可追溯(墓碑在 `history_horizon` 内保留,默认永久,经 `iter_with(..., true)` 可见)([07 §3.4](07-l5-life.md));
- **I24 更新原子可见**:`update` 的新版本对读者原子可见,旧版本立即遮蔽([03 §2.1](03-l1-memory.md));
- **I25 关系一致性**:悬挂边不可见,删除级联失效([09 §2](09-memory-model.md));
- **I26 双时态一致**:`as_of(t)` 结果 = 事务时间 ≤ t 的最新可见版本组成的一致快照,不随后续写入或 compaction 变化;历史版本默认永久保留,受 `CompactionPolicy.history_horizon` 约束([09 §3](09-memory-model.md)、[07 §4.2a](07-l5-life.md));
- **I27 反馈幂等**:同一 `(rowid, query_id)` 的反馈至多计一次([10 §4](10-scoring.md));
- **I28 加密不落明文**:开启加密后,磁盘上任何段/WAL/MANIFEST 不含明文记录字段([11 §2](11-security-storage.md));
- **I29 只读一致**:只读实例看到的始终是某个已提交 MANIFEST 版本的完整视图([12 §2](12-deployment.md));
- **I30 可观测无副作用**:`Observer` 回调不得改变引擎行为,回调 panic 被隔离([12 §4](12-deployment.md))。

验收方法见 [14 §2/§3/§6](14-testing.md) 与 [spec/contracts.md](../spec/contracts.md)。

---

## 10. 诊断与日志

Mneme **不引入 `log`/`tracing` 依赖**(与"依赖极简"一致),运行时诊断统一走结构化接口:

- `db.stats()`:段数/行数/WAL 尺寸/延迟直方图/每命名空间统计/量化状态/compaction 状态,
  宿主可定期采样上报;
- `db.check()`:全量 CRC + 索引一致性对账,与生产复用的是同一套校验器([14 §8](14-testing.md));
- 致命错误经 `MnemeError` 返回,**绝不 panic、绝不静默**([02 §2](02-l0-core.md));
- **可选事件流**:`Builder::observer(Arc<dyn Observer>)` 接收 Query/Write/Flush/Compaction/Error
  事件,宿主可桥接到 `tracing`/OpenTelemetry/metrics;默认不注册即零成本,回调 panic 被隔离
  ([12 §4](12-deployment.md)、不变量 I30);
- 若宿主需要日志,建议在调用边界记录 `MnemeError` 与 `Stats` 快照;引擎内部不打印。

> `Observer` 不引入 `log`/`tracing` 依赖;若确有需求,可再以可选 feature 提供官方桥接,
> 不改变默认零依赖承诺。

---

## 11. 容量与资源估算(速查)

以下为默认参数下的量级估算,用于容量规划(推导见各章):

| 项 | 1M × 1536 维 | 出处 |
|---|---|---|
| f32 向量(始终保留) | ≈ 6.1 GB(6144 B/行) | [05 §6.3](05-l3-hnsw.md) |
| i8 量化副本(额外) | ≈ 1.5 GB(1536 B/行) | [08 §1](08-l6-quant.md) |
| i8 模式段总存储 | ≈ 7.7 GB(5×1536 B/行 = 7680 B/行) | [08 §2.4](08-l6-quant.md) |
| HNSW 图(hidx) | ≈ 148 MB(≈148 B/节点) | [05 §6.3](05-l3-hnsw.md) |
| 查询带宽(暴力) | ≈ 6 GB/次;双通道 ~50 GB/s → 下限 ~120ms | [03 §4.3](03-l1-memory.md) |
| 写放大 | 平均 ≈ log_r(N/B) ≈ 7 次(上界 ≈ 9) | [07 §4.2](07-l5-life.md) |
| 活跃段数 | ≤ (T−1)·log_r(N/B) + c | [07 §4.2](07-l5-life.md) |
| 历史版本 | 随更新/删除次数线性增长;`history_horizon` 有限时仅保留窗口内版本 | [07 §4.2a](07-l5-life.md) |

文本与元数据原样存储、未计入(见 [01 §1.2](01-overview.md) 设计边界);实际磁盘占用
≈ 向量 + 图 + 文本 + 元数据 + trash 峰值。

## 本章小结

- 完整 API 清单与签名(L1 冻结)、`Builder` 配置总表与推荐预设。
- 打开已有库的校验规则、文件锁(OS 咨询锁与 `Busy` 语义);错误分类与重试建议。
- 线程安全保证、嵌入模型/框架集成方式、备份恢复 runbook、数据限额与保留字段。
- 诊断统一走 `stats()`/`check()`/`Observer`,不引 `log`/`tracing`。
- **本章定义**:I15、I16;并汇总 I17–I30 的面向使用者表述。

## 下一章

[spec/contracts.md](../spec/contracts.md):形式化契约矩阵(FC-Matrix)与测试追溯。
