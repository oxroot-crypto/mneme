# 12 部署形态:多进程只读、WASM 与可观测性

> **本章目标**:把 Mneme 从"单进程独占库"扩展到更广的部署场景——同机多进程只读共享、
> 浏览器/边缘(WASM)、以及生产可观测性,同时不破坏核心的简单性。
> **前置阅读**:[04 §6/§8](04-l2-persist.md)(MANIFEST 原子性/ReaderView)、[16 §3/§5](16-api-reference.md)(打开校验/线程安全)。
> **本章你将学到**:只读共享的可行性与边界 → 存储抽象与 WASM → 可选可观测钩子 → 层边界。
>
> 只读共享与 WASM 是**采用门槛级**能力:许多 Agent 框架会
> 以多进程运行,边缘/浏览器则是嵌入式库的天然战场。单写者语义保持不变。

模块:`persist/storage/`(`Storage`/`FsStorage`/`MemStorage` 与文件锁)、
`core/observe.rs`(`Observer`/`Event`/`WriteOp`);只读共享与周期探测落在 L1/L2
引擎路径。

---

## 1. 为什么需要多进程

多进程并发**写**是明确不做的(单写者);但**多进程只读**是另一回事:
一个 Agent 服务常有多个 worker 进程读同一份记忆,一个调试工具想在服务运行时查库。
完全不支持会迫使调用方自建复制,反而更复杂。

**结论**:引擎保持单写者;提供**只读共享模式**(`Builder::read_only(true)`),
写者仍独占,读者可多进程并发。这是 SQLite 式"WAL 只读连接"的轻量对应物。

---

## 2. 只读共享模式

### 2.1 语义

```rust
let db = Mneme::builder().path("./agent_memory").read_only(true).build()?;
// db 只能读;任何写操作返回 Unsupported { feature: "只读模式写入" }
```

- 只读实例**不创建/不争抢写锁文件**,也不探测写者存活([16 §3](16-api-reference.md));
- 只读实例**不改动文件系统**:打开时**不建目录、不清 `trash/`、不移动损坏段、不截断 WAL**,
  只读取既有数据;在内存中重放未落段的 WAL(遇撕裂帧只忽略、不截断,
  [04 §7](04-l2-persist.md));损坏段只从视图剔除,不移动文件(库目录不存在 → `Config`);
- 只读实例的可见性:打开时读一次 `current` → MANIFEST;注册探测周期
  (`Builder::read_only_probe_interval`,默认 1s)后后台线程周期探测 `current`
  并原子切换视图(`Table::publish`),也可显式调用 `Mneme::reload()`;
- **不变量 I29**:任意时刻只读实例看到的都是某个**已提交 MANIFEST 版本的完整视图**
  (段集一致,不会看到半提交状态);新版本的可见延迟 ≤ 探测周期(默认 1s,L12)。

### 2.2 为什么不需要协议

段文件是 **write-once**([04 §1](04-l2-persist.md))、MANIFEST 是 **write-once + 指针**([04 §6](04-l2-persist.md)):
写者只会新增文件、切换指针,从不原地修改已有文件。因此只读进程只要"看到某个指针,
就拥有一份不可变、自洽的数据集"——无需与写者通信,无需文件锁。这正是当初
"write-once"设计的额外红利。

### 2.3 边界与代价

| 项 | 说明 |
|---|---|
| 写者崩溃 | 只读实例仍可读到最后一个已提交版本;写者重启后继续 |
| 段被 compaction 删除 | 只读实例仍持有旧段的 `Arc`/句柄,Windows 上删除被推迟到引用释放([04 §9](04-l2-persist.md)) |
| 探测开销 | 每周期一次 stat/读 `current`,可忽略 |
| 不支持 | 多进程**写**(仍 `Busy`);跨主机共享(需网络文件系统的一致性保证,不在承诺内) |

### 2.4 复杂度

切换视图 = 重新打开 MANIFEST + mmap 新段,成本与冷启动同类(页缓存命中时极低);
旧视图随 `Arc` 释放。

---

## 3. 存储抽象与 WASM/边缘

### 3.1 现状

[04 §11](04-l2-persist.md) 已把段读取抽象为 `SegmentSource`(mmap / `FileSource`)。
WASM 没有 mmap,但有内存文件系统/IndexedDB;为此需要把**写路径**也抽象出来。
`Storage` trait 已定义在 `persist/storage/`(与 `SegmentSource` 同层,
避免上层反向依赖);WASM 适配由 `MemStorage`/宿主后端提供。
> `Storage` trait 与 `FsStorage`/`MemStorage` 定义在 `persist/storage/`,
> `Builder::storage` 可注入任意后端;段/WAL/MANIFEST/trash/恢复/校验全链路经后端
> 读写(WAL 写入器不持 `File` 句柄),`MemStorage` 覆盖完整生命周期测试
> (`FC-DEPLOY-POST-001`)。WASM 目标由 feature `wasm` 关闭 mmap 与后台线程、
> 配合 `MemStorage`/宿主后端;目标构建由 CI `wasm-check` job 验证。

```rust
/// 存储后端的文件元数据(不依赖 `std::fs`,WASM 后端同样可实现)。
pub struct FileMeta { pub len: u64 }

pub trait Storage: Send + Sync + std::fmt::Debug {
    fn read_file(&self, rel: &str) -> Result<Vec<u8>>;
    fn read_file_opt(&self, rel: &str) -> Result<Option<Vec<u8>>>;
    fn read_prefix(&self, rel: &str, max: usize) -> Result<Vec<u8>>;  // 段打开 4 字节信封探测;默认整读后截断,文件后端应覆写
    fn write_atomic(&self, rel: &str, bytes: &[u8]) -> Result<()>;  // 临时文件 + fsync + rename,绝不原地覆盖
    fn write_new(&self, rel: &str, bytes: &[u8]) -> Result<()>;     // 只创建不覆盖(已存在则失败)
    fn append(&self, rel: &str, bytes: &[u8]) -> Result<u64>;       // 追加并返回新长度(WAL 顺序写)
    fn truncate(&self, rel: &str, len: u64) -> Result<()>;
    fn sync(&self, rel: &str) -> Result<()>;
    fn list_dir(&self, rel: &str) -> Result<Vec<String>>;
    fn ensure_dir(&self, rel: &str) -> Result<()>;
    fn remove_if_exists(&self, rel: &str) -> Result<()>;
    fn rename(&self, from: &str, to: &str) -> Result<()>;
    fn exists(&self, rel: &str) -> Result<bool>;
    fn stat(&self, rel: &str) -> Result<FileMeta>;
    // ---- 默认实现(实现者通常无需覆写)----
    fn open_bytes(&self, rel: &str) -> Result<RawBytes>;            // 默认整文件读入;mmap 后端可覆写
    fn root_exists(&self) -> Result<bool>;
    fn try_lock(&self) -> Result<Box<dyn std::any::Any + Send + Sync>>;  // 独占锁;`Drop` 即释放
}
```

> `RawBytes` 是 `open_bytes` 的只读字节视图:`Mmap`(feature `mmap` 下的内核映射)
> 或 `Owned`(整文件自有缓冲);`Storage::open_bytes` 的默认实现返回 `Owned`,
> 实现者不必关心 mmap。返回 `Mmap` 的后端必须保证视图存活期内文件不被原地
> 改写/截断;库内仅对 write-once 段文件调用 `open_bytes`。

- 桌面/服务器用 `FsStorage`(std);WASM 用 `MemStorage`(纯内存)或宿主提供的
  `OpfsStorage`(Origin Private File System,经宿主实现);后端经 `Builder::storage(Arc<dyn Storage>)`
  注入([16 §1.1](16-api-reference.md)),默认 `FsStorage`;
- 文件锁经 `Storage` 提供 `try_lock` 等价能力(桌面 `FsStorage` 用 OS 咨询锁,见 [16 §3](16-api-reference.md)),存在性判定(`exists`)亦走 `Storage`,保证后端可替换;
- `read_only` + `MemStorage` 可用于浏览器内只读记忆;写入需宿主提供持久化策略。

### 3.2 `no_std + alloc` 核心

- `core`(L0)天然无 I/O,改为 `no_std + alloc` 友好;
- `persist` 定义并依赖 `Storage`/`SegmentSource` trait(不直接依赖 `std::fs` 具体类型);
  `index`/`query`/`life` 经 L2 的段抽象读取,不自行触碰文件系统;
- feature `wasm` 关闭 mmap/线程并行(用单线程 fallback),保留正确性;`std::thread::scope`
  的并行扫描在无线程环境退化为顺序扫描。

### 3.3 边界

WASM 通过 `Storage` 抽象接入;本章固化 `Storage`/`SegmentSource` 抽象与
"无 mmap 可退化"的约束。

---

## 4. 可观测性:`Observer`

### 4.1 设计

[16 §10](16-api-reference.md) 提供 `stats()` 轮询;事件级可观测由 `Observer` 提供,
同时守住"默认不引 `log`/`tracing` 依赖"的边界。

> `Observer`/`Event`/`WriteOp`/`ErrorKind` 定义于 `src/core/observe.rs`,
> `Builder::observer` 注册;查询/写入/flush/compaction/错误均发事件,回调 panic 经
> `catch_unwind` 隔离(FC-DEPLOY-INV-030)。

```rust
pub trait Observer: Send + Sync {
    fn on_event(&self, event: Event);   // 默认空实现
}
pub enum Event {
    Query { took: Duration, candidates: usize, returned: usize, channels: u8 },
    Write { op: WriteOp, took: Duration, bytes: usize },
    Flush { segments: usize, wal_bytes: u64 },
    Compaction { segments: Vec<SegmentId>, took: Duration, rows_out: u64 },
    Error { kind: ErrorKind, context: &'static str },
}
pub enum WriteOp { Insert, InsertBatch, Update, Delete, Touch, Relate, Unrelate, Forget, Retain, Supersede, Consolidate, DropNamespace }
pub enum ErrorKind { Io, Corrupted, Busy, TooLarge, LimitExceeded, UnsupportedVersion, Closed, Config, Unsupported, Other }
```

> `ErrorKind` 是 `MnemeError` 的粗分类:同名变体直接对应;`DimensionMismatch`/`MetricMismatch`/
> `KeyMismatch`/`DuplicateKey`/`FilterParse`/`KeyNotFound`/`NonFinite`/`MetaTooDeep`/`Inconsistent` 归入 `Other`
> (需要精确定位时宿主仍以 `MnemeError` 为准,见 [02 §2](02-l0-core.md)、[16 §4](16-api-reference.md))。

- 默认 `None`(不注册即零成本);宿主可把事件桥接到 `tracing`/OpenTelemetry/metrics;
- **不变量 I30**:回调**不得改变引擎行为**;回调 panic 被 `catch_unwind` 隔离并忽略
  (引擎自身绝不因观测而失败);
- 事件为**采样友好**:高频 `Query` 事件可由宿主自行降采样,引擎不做背压。

### 4.2 与 `stats()` 的关系

`stats()` 是**快照**(当前段/WAL/延迟直方图/compaction 状态),`Observer` 是**流**;
两者互补。延迟直方图仍由引擎维护([07 §7](07-l5-life.md)),`Observer` 提供单次事件的细节。

---

## 5. 层边界契约(产品能力层 → 外部)

**向上提供**:

1. `read_only` 模式与多进程只读一致视图(不变量 I29);
2. `Storage`/`SegmentSource` 抽象与 WASM/`no_std` 退化路径;
3. `Observer` 事件钩子(不变量 I30)。

**依赖**:L0(类型)、L2(MANIFEST/段/write-once 语义)。

**不变量**:I29(只读一致)、I30(可观测无副作用);单写者语义不变。

## 本章小结

- 多进程**只读**共享:靠 write-once + 指针,无需与写者通信或文件锁(I29)。
- `Storage`/`SegmentSource` 抽象支持 WASM/边缘与 `no_std + alloc` 退化。
- `Observer` 事件流与 `stats()` 快照互补;回调不得改变引擎行为、panic 被隔离(I30)。
- 单写者语义始终不变。
- **本章不变量**:I29(只读一致)、I30(可观测无副作用)。

## 下一章

[13-cookbook.md](13-cookbook.md):Agent 记忆模式配方(可直接照抄)。
