# 11 公开 API 与运维参考

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

### 1.1 构建与打开

```rust
impl Mneme {
    /// 新建/打开一个本地目录记忆库。等价于 builder().path(dir).build()。
    /// 目录已存在时,维度与度量从 MANIFEST 读回,无需重复指定。
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
    pub fn dedup_threshold(self, t: f32) -> Self;           // 默认 0.95;近似去重余弦阈值
    pub fn quantization(self, f: VectorFormat) -> Self;     // 默认 F32;见 08
    pub fn hnsw(self, p: HnswParams) -> Self;               // 默认 M=16/M0=32/efc=200/ef=64
    pub fn compaction(self, p: CompactionPolicy) -> Self;   // 默认见 §2
    pub fn retention(self, r: Option<Retention>) -> Self;   // 后台自动遗忘;None 关闭(见 07 §3.4)
    pub fn retain_interval(self, d: Duration) -> Self;      // 默认 半衰期/4
    pub fn access_flush_interval(self, d: Duration) -> Self;// 默认 30s
    pub fn parallelism(self, n: usize) -> Self;             // 默认 0 = available_parallelism()
    pub fn tuning(self, t: Tuning) -> Self;                 // 进阶调参,默认见 §2
    pub fn limits(self, l: Limits) -> Self;                 // 数据限额,见 §8
    pub fn clock(self, c: Arc<dyn Clock>) -> Self;          // 测试注入;默认 SystemClock
    pub fn build(self) -> Result<Mneme>;
}
```

### 1.2 写入

```rust
impl Namespace {
    /// 单条写入。语义见 03 §2.1。
    pub fn insert(&self, rec: Record) -> Result<InsertOutcome>;

    /// 批量写入,单次原子:要么整批可见,要么整批不可见(不变量 I15)。
    /// 整批共用一次 fsync 组提交;任一条校验失败 → 整批拒绝,不产生部分写入。
    pub fn insert_batch(&self, recs: Vec<Record>) -> Result<Vec<InsertOutcome>>;

    /// 显式删除。返回是否命中活记录。
    pub fn delete(&self, key: &str) -> Result<bool>;

    /// 按全局稳定 RowId 删除(无 key 的记录也适用)。返回是否命中活记录。
    pub fn delete_by_rowid(&self, id: RowId) -> Result<bool>;
}

impl Record {
    pub fn new(vector: Vec<f32>) -> Self;                   // 维度在 insert 时校验,故无 Result
    pub fn key(self, k: impl Into<String>) -> Self;         // 可选外部键
    pub fn text(self, t: impl Into<String>) -> Self;        // 可选;启用 BM25/文本去重
    pub fn metadata(self, m: Meta) -> Self;                 // 可选 JSON
    pub fn ttl(self, d: Duration) -> Self;                  // 可选;默认永不过期
    pub fn importance(self, v: f32) -> Self;                // 可选;默认 0.5,越界钳制
}

/// 写入结果。`Duplicate` 在 `Dedup::Reject` 时返回;`existing` 用 RowId 而非 Key,
/// 因为近似去重命中的记录可能没有 key(Key 可由 `get_by_rowid` 取回)。
pub enum InsertOutcome {
    Inserted(RowId),
    Duplicate { existing: RowId, score: f32 },
}

/// 检索命中的物化视图(只读)。**只用于带查询的 `search()`**。
/// `score` 是 `Metric::score` 的原始值:Dot/Cosine 越大越相似,Euclidean 为距离平方
/// (越小越近);命中列表一律按 `Metric::better` 排序(最优在前),同分按 `rowid` 升序。
/// 注意:为控制内存,`Hit` 不携带原始向量;需要向量时用 `get_vector(rowid)`。
pub struct Hit {
    pub rowid: RowId,              // 全局稳定记录标识(见 02 §1)
    pub key: Option<Key>,          // 写入时未给 key 则为 None
    pub score: f32,
    pub created_at: i64,           // Unix 毫秒
    pub expires_at: Option<i64>,   // None = 永不过期
    pub importance: f32,
    pub text: Option<String>,
    pub metadata: Meta,
}

/// 存储记录的只读视图,**用于 `get` / `iter`(没有查询,故没有 `score`)**。
/// `vector()` 返回原始 f32 向量;需要高吞吐批量取向量时用 `get_vector`。
pub struct RecordRef {
    pub rowid: RowId,
    pub key: Option<Key>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub importance: f32,
    pub text: Option<String>,
    pub metadata: Meta,
}
impl RecordRef {
    pub fn vector(&self) -> &[f32];
}

```

### 1.3 读取

```rust
impl Namespace {
    pub fn search(&self) -> SearchBuilder;                  // 见下
    pub fn get(&self, key: &str) -> Result<Option<RecordRef>>;    // 快照一致的单点读
    pub fn get_by_rowid(&self, id: RowId) -> Result<Option<RecordRef>>;

    /// 单独取回原始向量(不物化整条记录;批量场景比逐条 get 更省内存)。
    pub fn get_vector(&self, id: RowId) -> Result<Option<Vec<f32>>>;

    /// 统计命中行数(不物化记录,复用过滤器/索引)。
    pub fn count(&self, filter: Option<Expr>) -> Result<u64>;

    /// 按过滤条件流式遍历(导出/审计/重建用),快照一致;不参与 ANN。
    /// 外层 `Result` 是"建立遍历"的错误;内层 `Result` 是逐行读取(段损坏/被删)的错误——
    /// 迭代中途的 I/O 失败必须能被调用方看见,绝不静默截断(I2)。
    pub fn iter(
        &self,
        filter: Option<Expr>,
    ) -> Result<impl Iterator<Item = Result<RecordRef>> + '_>;
}

impl SearchBuilder<'_> {
    pub fn vector(self, q: &[f32]) -> Self;                 // 向量通道
    pub fn text(self, q: &str) -> Self;                     // BM25 通道
    pub fn top_k(self, k: usize) -> Self;                   // 默认 10,上限 4096
    pub fn ef(self, ef: usize) -> Self;                     // 仅 L3+ 生效;上限 4096
    pub fn filter(self, e: Expr) -> Self;                   // 预过滤(语义见 03 §2.2)
    pub fn dedup(self, d: ResultDedup) -> Self;             // 结果级去重(见 06 §6)
    pub fn fusion(self, f: Fusion) -> Self;                 // 默认 Rrf{k:60}
    pub fn rerank(self, r: Arc<dyn Reranker>) -> Self;      // 可选精排钩子
    pub fn execute(&self) -> Result<Vec<Hit>>;              // 至少一个通道非空,否则 Invalid
}
```

### 1.4 生命周期

```rust
impl Namespace {
    pub fn touch(&self, key: &str, boost: Option<f32>) -> Result<bool>;  // 访问计数 +1;boost=Some(d) 时 importance += d
    pub fn touch_by_rowid(&self, id: RowId, boost: Option<f32>) -> Result<bool>;
    pub fn forget(&self, filter: Expr) -> Result<usize>;    // 主动遗忘,返回删除数
    pub fn retain(&self, policy: Retention) -> Result<RetainReport>;
}
```

### 1.5 命名空间

```rust
impl Mneme {
    pub fn namespace(&self, path: &str) -> Namespace;               // 不存在则隐式创建
    pub fn list_namespaces(&self) -> Result<Vec<String>>;           // 前缀树顺序
    pub fn drop_namespace(&self, path: &str) -> Result<usize>;      // 含所有子命名空间
}
```

### 1.6 快照与运维

```rust
impl Mneme {
    pub fn snapshot(&self) -> SnapshotHandle;   // 钉住当前 ReaderView(含可变表快照)
    pub fn backup_to(&self, dir: impl AsRef<Path>) -> Result<BackupReport>;  // 先 flush 再备份
    pub fn stats(&self) -> Result<Stats>;
    pub fn check(&self) -> Result<CheckReport>;  // fsck:CRC + 索引一致性 + 对账
    pub fn compact_control(&self) -> CompactionControl;  // pause()/resume()/state()
    pub fn flush(&self) -> Result<()>;           // 把可变表落成段并 fsync WAL
    pub fn close(self) -> Result<()>;            // flush + 释放文件锁;幂等(对已关闭的库经其他句柄再调返回 Ok)
}

impl SnapshotHandle {
    pub fn version(&self) -> u64;                          // 对应 MANIFEST 版本
    pub fn search(&self) -> SearchBuilder;                 // 在钉住的视图上查询
    pub fn get(&self, key: &str) -> Result<Option<RecordRef>>;
    pub fn get_by_rowid(&self, id: RowId) -> Result<Option<RecordRef>>;
    pub fn get_vector(&self, id: RowId) -> Result<Option<Vec<f32>>>;
    pub fn iter(&self, filter: Option<Expr>)
        -> Result<impl Iterator<Item = Result<RecordRef>> + '_>;
    pub fn stats(&self) -> SnapshotStats;
}
```

```rust
/// 遗忘策略(写入期/后台共用)。`Retention::new()` 默认 half_life=14d、min_importance=0.2。
pub struct Retention {
    pub half_life: Duration,
    pub min_importance: f32,
    pub protect: Option<Expr>,          // 白名单:命中者豁免
}
impl Retention {
    pub fn new() -> Self;
    pub fn half_life(self, d: Duration) -> Self;
    pub fn min_importance(self, v: f32) -> Self;
    pub fn protect(self, e: Expr) -> Self;
}

/// 融合器。`Rrf` 默认 k=60;`Weighted` 的 alpha 默认 0.5(见 06 §4)。
pub enum Fusion { Rrf { k: u32 }, Weighted { alpha: f32 } }

/// 运维报告类型(字段为稳定契约)。
pub struct RetainReport { pub scanned: usize, pub forgotten: usize }
pub struct BackupReport { pub files: usize, pub bytes: u64, pub hardlinked: bool }
pub struct CheckReport  { pub ok: bool, pub corrupted: Vec<SegmentId>, pub suggestions: Vec<String> }
pub struct SnapshotStats { pub version: u64, pub segments: usize, pub rows: u64 }

/// 后台合并状态(`stats()` 的 `compaction` 字段)。
pub enum CompactionState {
    Idle,
    Running { progress: f32, segments: Vec<SegmentId> },
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
- `Drop` 只**尽力** `flush`(忽略错误,无法向调用方报告),并释放锁;
  需要确保持久性时不要依赖析构。
- 进程被 `abort`/断电时,已 fsync 的写入仍由 WAL 恢复([04 §7](04-l2-persist.md))。

---

## 2. Builder 与配置总表

所有参数在设计稿中给出的都是**默认值**,最终以 `Builder` 配置项暴露。

| 配置 | Builder 方法 | 默认 | 作用 / 详见 |
|---|---|---|---|
| 存储路径 | `.path` | 无(纯内存) | 目录 = 一个库 |
| 维度 | `.dimension` | 新建必填 | 1..=65536,建库后不可改 |
| 度量 | `.metric` | `Cosine` | `Cosine/Dot/Euclidean`,[02 §3](02-l0-core.md) |
| fsync 策略 | `.fsync` | `Batched(20ms)` | `Always/Batched/OnFlush/Never`,[04 §3](04-l2-persist.md) |
| 同 key 行为 | `.insert_mode` | `Upsert` | `Upsert/RejectDuplicate` |
| 去重策略 | `.dedup` | `Off` | `Off/Reject/Replace/KeepBoth/Merge`,[03 §6](03-l1-memory.md) |
| 去重阈值 | `.dedup_threshold` | `0.95` | 近似去重的余弦阈值(与 `ResultDedup::Near` 独立) |
| 量化格式 | `.quantization` | `F32` | `F32/F16/I8Rescored`,[08](08-l6-quant.md) |
| HNSW 参数 | `.hnsw` | 见下 | `HnswParams` |
| compaction | `.compaction` | 见下 | `CompactionPolicy` |
| 自动遗忘 | `.retention` | 开(14d/0.2) | `Option<Retention>`;`None` 关闭后台 retain,见 [07 §3.4](07-l5-life.md) |
| 遗忘扫描周期 | `.retain_interval` | 半衰期/4 | 后台 retain 的触发间隔 |
| 访问统计落盘 | `.access_flush_interval` | `30s` | 内存访问计数批量写 WAL 的周期,见 [07 §2](07-l5-life.md) |
| 并行度 | `.parallelism` | `0`(自动) | 扫描/建索引/合并的线程数;0 = `available_parallelism()` |
| 进阶调参 | `.tuning` | 见下 | `Tuning` |
| 数据限额 | `.limits` | 见 §8 | `Limits` |
| 时钟 | `.clock` | `SystemClock` | 测试注入,见 [04 §10](04-l2-persist.md) |

```rust
pub struct HnswParams {
    pub m: u16,              // 默认 16  上层度数上限
    pub m0: u16,             // 默认 32  第 0 层度数上限
    pub ef_construction: u16,// 默认 200 构建探查宽度
    pub ef_search: u16,      // 默认 64  查询探查宽度
}

pub struct CompactionPolicy {
    pub tier_ratio: u32,     // 默认 4   分级比 r
    pub tier_count: u32,     // 默认 4   同层合并阈值 T
    pub dead_ratio: f32,     // 默认 0.25 墓碑+过期占比触发线
    pub wal_bytes: u64,      // 默认 256MB WAL 压力触发线
    pub wal_file_bytes: u64, // 默认 64MB  单个 WAL 文件轮转阈值(04 §3.2)
    pub segment_rows: u64,   // 默认 8192  段初始目标行数 B(07 §4.2)
    pub io_budget: f32,      // 默认 0.30 后台合并磁盘配额
}

/// 进阶调参(通常保持默认;调大以小段增多/内存为代价)。
pub struct Tuning {
    pub parallel_block: usize,          // 默认 8192  暴力扫描的计算分块粒度(03 §4.2)
    pub field_dict_max: u16,            // 默认 16    每段可索引字段上限(04 §5.1)
    pub bloom_fpp: f32,                 // 默认 0.01  布隆过滤器目标误判率(04 §5.3)
    pub brute_force_max_rows: u32,      // 默认 2048  段行数低于此值恒用暴力(05 §9)
    pub filter_post_threshold: f32,     // 默认 0.10  过滤三档:后过滤/约束遍历分界(05 §8)
    pub filter_brute_threshold: f32,    // 默认 0.001 过滤三档:约束遍历/候选暴力分界(05 §8)
    pub stopwords: bool,                // 默认 true  启用内置停用词表(06 §3.5)
}
```

### 2.1 推荐预设

| 场景 | 建议配置 |
|---|---|
| 会话级临时记忆 | `Record::ttl` 短 + `FsyncPolicy::Batched(20ms)` + 默认 `retention`;命名空间按会话分 |
| 长期偏好/事实 | `importance` 显式设高 + `Retention::min_importance` 提高 + `Dedup::Replace` 或 `Merge` |
| 只读/分析副本 | `.fsync(Never)` 仅限测试;生产只读副本仍用 `Batched`,备份目录 `open` 后勿写 |
| 延迟敏感 | `.quantization(I8Rescored)` + `ef=64~128`;`.parallelism(0)` 交给运行时 |
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
4. 单进程独占:目录已被本进程或其他进程打开 → Busy(文件锁,见 §4)
```

**空目录 / 半初始化目录**:目录存在但没有 `current`/`MANIFEST.*` 时,视为**新建**
(`.dimension()` 必填);若只有 `current` 而无对应 MANIFEST(初始化在写 current 前崩溃),
按 [04 §6](04-l2-persist.md) 扫描合法 MANIFEST,全无则同样按新建处理。
若目录里有无法识别的残留文件但不含 `current`/`MANIFEST.*`,仍按新建;含 `current`
但 MANIFEST 全坏则报 `Corrupted`,绝不覆盖已有数据。

> **设计理由**:记忆库的维度是数据的一部分,不能由调用方每次"猜"。
> 让 MANIFEST 成为唯一事实来源,避免"同目录两种维度"导致的静默错读。
>
> **文件锁实现**:打开时在目录下以 `create_new` 原子创建锁文件(跨平台通用),
> 写入 `{ pid, 启动时间戳 }` 后持有其句柄;进程退出/`close()` 时删除。第二个实例
> 创建失败时**读取锁文件并校验持锁进程是否存活**(跨平台探活;无法探活的平台回退到
> 时间戳 + 租约超时):持有者已死 → 视为陈旧锁并接管,否则返回 `Busy`。
> 由此崩溃不会留下永久锁死(见 [04 §13](04-l2-persist.md))。
> 无需依赖平台特有的 flock/LockFileEx。

维度、度量之外的**可变配置**(fsync、去重、量化、HNSW、compaction、limits)
每次打开时可自由调整,不构成不兼容。

---

## 4. 错误处理与重试

统一错误类型见 [02 §2](02-l0-core.md)。按"能否重试"分类:

| 错误 | 可重试? | 处置建议 |
|---|---|---|
| `Io` | ✅ 通常可 | 瞬时 I/O 抖动;指数退避重试。`ErrorKind::Other`/`NotFound` 需先排查环境 |
| `Busy` | ✅ 可 | 另一实例持有文件锁或正在备份;退避后重试,或确保单进程独占 |
| `DuplicateKey` | ✅ 可 | 改用 `InsertMode::Upsert`,或先 `get` 再决定 |
| `KeyNotFound` | ❌ | **当前无 API 产生**(保留变体);`get` 返回 `Option`、`delete`/`touch` 返回 `bool`,均不以缺失报错 |
| `DimensionMismatch` | ❌ | 调用方 bug:向量长度 ≠ 建库维度 |
| `MetricMismatch` | ❌ | 打开参数与库不一致;去掉显式 metric 或改对 |
| `FilterParse` | ❌ | DSL 语法错误,错误携带位置;修正表达式 |
| `Invalid` / `TooLarge` | ❌ | 参数或数据超限,见 §8 |
| `UnsupportedVersion` | ❌ | 库由更新版本的 Mneme 写入;升级库,勿降级读 |
| `Corrupted` | ❌ | 数据损坏:立即停止写入,跑 `db.check()`,按 §7 恢复 |

**原则**:错误信息面向排查——`Corrupted` 带段号与原因,`FilterParse` 带出错位置
([02 §2](02-l0-core.md))。库本身**绝不 panic**;唯二的例外是:① `async` 门面中
`spawn_blocking` 任务被取消/panic 的 `expect`([08 §6](08-l6-quant.md));
② `filter!` 宏对写死的非法字面量在展开处 panic——运行时输入请用 `Expr::from_str`
返回的 `Result`([06 §1.1](06-l4-query.md)、不变量 I7)。

---

## 5. 线程安全与 `Send + Sync`

| 类型 | `Send` | `Sync` | 说明 |
|---|---|---|---|
| `Mneme` / `Namespace` | ✅ | ✅ | 内部 `Arc`;克隆廉价,可跨线程共享 |
| `SnapshotHandle` | ✅ | ✅ | 只读视图,任意线程并发查询 |
| `SearchBuilder` | ✅ | ❌ | 短生命周期构建器,通常单线程用完即 `execute` |
| `Record` / `Hit` / `InsertOutcome` | ✅ | ✅ | 值类型 |
| `Reranker` / `Clock` | ✅ | ✅ | 宿主实现需满足 |

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
db.backup_to("./backup")?;   // 一致性快照:硬链接同版本段文件(跨盘回退复制)
```

**目标目录语义**:目标必须不存在或为空;若已含库内容则返回 `Invalid`(不合并、不覆盖),
避免把两次备份混在一起。备份写入是"先写全部文件、最后写 `current`/MANIFEST"的顺序,
中途失败会留下一个不含合法 `current` 的目录——它无法被打开,重新备份即可(不污染源库)。
`BackupReport.hardlinked` 标明是否走了硬链接路径。

备份目录是一个**可独立打开**的完整库;验证:

```rust
let b = Mneme::open("./backup")?;
b.check()?;                  // 全绿 = 备份有效(写进 CI,见 09 §6)
```

### 7.2 时间点恢复(PITR)

MANIFEST 采用 write-once 版本文件并保留最近 2 个版本([04 §6](04-l2-persist.md))。
若需要回到"上一个提交点",在备份目录中把 `current` 指向上一个版本号即可:

```text
backup/MANIFEST.000041   ← 上一个版本(保留)
backup/MANIFEST.000042   ← 最新版本
backup/current           ← 内容 "42";改成 "41" 即回滚一个提交点
```

回滚前请先对当前备份整体复制一份,避免误操作不可逆。

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

超限一律返回 `TooLarge { field, limit, got }` 或 `Invalid`,**绝不静默截断**。

| 项 | 默认上限 | 备注 |
|---|---|---|
| 维度 | 65536 | 新建时校验 |
| key 长度 | 1024 字节 | UTF-8 编码后 |
| text 长度 | 1 MiB | 超出拒绝;BM25 对超长文本收益低 |
| metadata JSON | 64 KiB | 超出拒绝 |
| metadata 嵌套深度 | 32 层 | 防解析栈溢出 |
| `top_k` | 4096 | [03 §2.2](03-l1-memory.md) |
| `ef` | 4096 | 仅 L3+ |
| WAL 单帧 payload | 16 MiB | 撕裂写检测与内存上界 |
| 命名空间深度 | 32 级 | `a/b/c/...`,对应 `Limits.ns_depth` |

限额通过 `.limits(Limits { .. })` 调整;调大以内存/恢复时间为代价,请评估后再改。

> **向量取值校验**:插入时逐个分量检查是否为有限值,`NaN`/`±Inf` 一律返回 `Invalid`
> (否则会污染 `Metric::better` 的排序与 TopK)。零向量合法:余弦按 [02 §3.3](02-l0-core.md)
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
- **I18 版本兼容**:只接受主版本 ≤ 本库支持上界的文件(`major(format_version) ≤ max`);
  更高主版本返回 `UnsupportedVersion`,拒绝打开而非静默误读([04 §12](04-l2-persist.md))。

验收方法见 [09 §2/§3/§6](09-testing.md)。

---

## 10. 诊断与日志

Mneme **不引入 `log`/`tracing` 依赖**(与"依赖极简"一致),运行时诊断统一走结构化接口:

- `db.stats()`:段数/行数/WAL 尺寸/延迟直方图/每命名空间统计/量化状态/compaction 状态,
  宿主可定期采样上报;
- `db.check()`:全量 CRC + 索引一致性对账,与生产复用的是同一套校验器([09 §8](09-testing.md));
- 致命错误经 `MnemeError` 返回,**绝不 panic、绝不静默**([02 §2](02-l0-core.md));
- 若宿主需要日志,建议在调用边界记录 `MnemeError` 与 `Stats` 快照;引擎内部不打印。

> 未来若确有需求,计划以**可选 feature**(如 `tracing`)追加,不改变默认零依赖承诺。

## 上一章

[10-glossary.md](10-glossary.md):术语、符号与复杂度速查。
