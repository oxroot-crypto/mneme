# Mneme 的 Rust 零基础教学

> **本套文档写给谁**:完全没写过 Rust、但会用至少一门其他语言(如 Python/Java/C/Go/JavaScript)
> 写代码的人。你不需要任何 Rust 基础,也不需要 AI 或数据库背景。
>
> **本套文档想解决什么**:让你能**独立读懂 `mneme` 的源码**。本套以 L0 原语层
> `src/core/` 为教材,并把 L1 内存引擎 `src/memory/`、L2 持久层 `src/persist/`、
> L3 索引层 `src/index/`、L4 检索层 `src/query/`、L5 生命周期层 `src/life/`
> 与 L6 打磨层 `src/quant/` + async 门面(`src/memory/async_facade/`)
> 新引入的 Rust 知识(`Arc` 共享所有权、写时复制,`Mutex`/`RwLock` 守卫、原子类型与
> CAS,`std::thread` 后台线程、`Weak` 弱引用与 `Condvar`,`thread::scope` 借用式并行,
> `Drop`/RAII,`move` 闭包写事务,`dyn` 策略与函数指针,构建者模式,手写 `Ord` 与
> `BinaryHeap`,`TryFrom`/受检运算,字节切片操作,`std::io` 文件读写与 `ErrorKind`,
> `memmap2` 的 `unsafe`,`let` 链与 `let...else`,递归枚举与 `Box`,带生命周期的
> 解析器与切片借用,`PhantomData`,`HashMap` entry API,运算符重载,`Option` 组合子,
> `fmt::Write`,`thread_local!` 测试探针,`proptest` 自定义策略,`half::f16` 半精度,
> `async`/`.await`/`Future`,`tokio::task::spawn_blocking`,criterion 基准与 fuzz 骨架)
> 分散在各章对应小节(见 §4 对照表);L1–L6 的业务语义与分文件阅读路线见
> [设计 03 L1 内存引擎](../design/03-l1-memory.md)、
> [设计 04 L2 持久层](../design/04-l2-persist.md)、
> [设计 05 L3 HNSW](../design/05-l3-hnsw.md)、
> [设计 06 L4 检索层](../design/06-l4-query.md)、
> [设计 07 L5 生命周期层](../design/07-l5-life.md) 与
> [设计 08 L6 打磨层](../design/08-l6-quant.md)。
> 所有语法点都锚定在 mneme 的真实代码上,不讲"为了教语法而教语法"的空例子。
>
> **预计阅读**:7–11 小时(边读边敲会更快掌握)。建议**开着源码对照阅读**。

---

## 1. 为什么要专门写一套 mneme 的 Rust 教程

Rust 的通用教材很多(见 §5),但它们有两个问题:

1. **例子和 mneme 无关**。你学完了 `struct Point { x: i32 }`,却还是看不懂
   `TopK<T: Ord>` 为什么要写 `<T: Ord>`。
2. **学到的顺序和读源码的顺序不一致**。读 mneme 的 `src/core/` 时,你第一个撞上的
   不是"打印 Hello World",而是 `#![deny(missing_docs)]`、`#[derive(Debug, Clone, Copy)]`、
   `Arc<str>`、`thiserror` 这些"教材后半段才讲"的东西。

本套文档反过来:**以 mneme 源码为教材**,按"读懂它需要什么就讲什么"的顺序组织。
每一节都给出源码位置(`文件:行号`),你可以在编辑器里跳过去对照。

---

## 2. 怎么读这套文档

- **顺序读**:01 → 11。硬依赖不跳步(少量前瞻链接只是类比预告)。
- **边读边跑**:每章末尾有「动手练习」,在 `examples/` 下新建一个文件敲一遍(或用临时 crate)。
  **不要直接改 `src/core/`、`src/memory/`、`src/persist/`、`src/index/`、`src/query/`、`src/life/` 与 `src/quant/`**:库里的公开项受 `#![deny(missing_docs)]` 约束,乱加还会污染源码。
  光看不敲,Rust 的所有权和借用是学不会的。
- **对照源码**:遇到 `文件:行号` 就跳过去看完整上下文。
- **不要背语法**:Rust 编译器报错信息极其友好,学会"看报错 → 改代码"比背规则更重要。
  多数章都列了「你会遇到的编译器报错」(01 章的报错入门见 §5.1;03 章未单列,遇到时按
  04/05 两章的方法处理)。

> 阅读前请先确认本机已装 Rust 工具链;没有的话见 [01 工具链](01-toolchain.md) 的安装小节。

---

## 3. 章节地图

| 章 | 标题 | 一句话 | 读完后你能看懂 mneme 里的…… |
|---|---|---|---|
| [01](01-toolchain.md) | 工具链与 Cargo | `cargo` 怎么构建、测试、出文档 | `Cargo.toml`、edition、MSRV、`cargo test/doc` |
| [02](02-values-and-ownership.md) | 值、类型与所有权 | Rust 最独特、也最容易卡住的部分 | `u32`/`f32`、`mut`、移动/复制/克隆、newtype、`Arc`/写时复制、`TryFrom`/受检运算 |
| [03](03-structs-enums-impl.md) | 结构体、枚举与 impl | 用类型描述数据、用 `impl` 挂行为 | `RowId`、`Metric`、`MnemeError`、`#[derive]`、`Builder` 链、手写 `Ord`(`Cand`) |
| [04](04-borrowing-strings-slices.md) | 引用、借用、生命周期与字符串 | `&`、`&mut`、`&str`/`String`/`Arc<str>` | `dot(a, b)`、`Key`、`get_path` 的 `'v`、锁守卫、hidx 的字节切片读写 |
| [05](05-errors.md) | 错误处理 | `Option`/`Result`/`?`/`thiserror` | `MnemeError`、`Result<T>`、varint 的畸形输入、`let...else` 与 let 链 |
| [06](06-generics-traits.md) | 泛型与 trait | 一套代码适配多种类型 | `TopK<T: Ord>`、`Clock`、`From`/`Display`、`dyn` 策略 |
| [07](07-iterators-closures.md) | 迭代器与闭包 | 用链式调用替代手写循环 | `dot_scalar`、`TopK::into_sorted_vec`、`write_tx` 闭包、`BinaryHeap` 图搜索 |
| [08](08-modules-docs.md) | 模块、可见性与文档 | 代码怎么分文件、怎么暴露 | `lib.rs`、`core/mod.rs`、`//!` 与 `///` |
| [09](09-cfg-unsafe-simd.md) | 条件编译、unsafe 与 SIMD | 跨平台与手写向量指令 | `simd/` 的 `#[cfg(target_arch)]`、`unsafe` |
| [10](10-testing.md) | 测试与属性测试 | `#[test]`、doctest、`proptest`、criterion 与 fuzz | `tests/core_contracts.rs`、契约测试、proptest 自定义策略、复杂度探针、`benches/quant.rs`、`fuzz/` |
| [11](11-async-tokio.md) | 异步与 tokio 最小封装 | `async`/`.await`、future 与阻塞线程池 | `AsyncNamespace`、`spawn_blocking`、`no_run` doctest、取消语义 |

> 章节编号是稳定标识,不严格等于阅读次序;但**首次学习请按编号顺序**。

---

## 4. 与 mneme 源码的映射总表

下表把 mneme L0 的每个源文件映射到"你需要哪几章"。可以当作**查漏工具**:
读某个文件卡住时,回到对应章节。

| 源码文件 | 主要 Rust 知识点 | 对应章节 |
|---|---|---|
| `Cargo.toml` | package、依赖、edition、MSRV、profile | [01](01-toolchain.md) |
| `src/lib.rs` | crate 根、模块声明、`pub use`、crate 级属性 | [01](01-toolchain.md)、[08](08-modules-docs.md) |
| `src/core/types.rs` | newtype、`derive`、`const fn`、`From`/`Display`、`Arc<str>` | [02](02-values-and-ownership.md)、[03](03-structs-enums-impl.md)、[04](04-borrowing-strings-slices.md)、[06](06-generics-traits.md) |
| `src/core/error.rs` | `enum`、`thiserror`、`#[from]`、`#[non_exhaustive]`、`Result` 别名 | [03](03-structs-enums-impl.md)、[05](05-errors.md) |
| `src/core/metric.rs` | `enum`、方法、`match`、`matches!`、常量、doctest | [03](03-structs-enums-impl.md)、[05](05-errors.md)、[10](10-testing.md) |
| `src/core/simd/` | `unsafe`、`#[cfg]`、`#[target_feature]`、切片、迭代器 | [04](04-borrowing-strings-slices.md)、[07](07-iterators-closures.md)、[09](09-cfg-unsafe-simd.md) |
| `src/core/heap/` | 泛型 + trait bound、`Vec`、闭包、`Ordering`、`Option` | [02](02-values-and-ownership.md)、[06](06-generics-traits.md)、[07](07-iterators-closures.md) |
| `src/core/varint.rs` | 位运算、`Result`、`?`、迭代器、边界校验 | [02](02-values-and-ownership.md)、[05](05-errors.md)、[07](07-iterators-closures.md) |
| `src/core/meta.rs` | 类型别名、生命周期、`Option` 链、`serde_json` | [04](04-borrowing-strings-slices.md)、[05](05-errors.md) |
| `src/core/options/*.rs` | `struct`、`enum`、`Default`、`#[default]`、嵌套 `Option` | [03](03-structs-enums-impl.md)、[06](06-generics-traits.md) |
| `src/core/mod.rs` | 模块组织、`//!` 模块文档 | [08](08-modules-docs.md) |
| `tests/core_contracts.rs` | 集成测试、`proptest`、契约追溯 | [10](10-testing.md) |

L1(`src/memory/`)引入的 Rust 特性落在以下章节:

| L1 新特性 | 落在哪一节 |
|---|---|
| `Arc<T>` 共享所有权、`Arc::make_mut` 写时复制 | [02 §3.7](02-values-and-ownership.md) |
| 链式构建者模式(`mut self -> Self`) | [03 §2.2.3](03-structs-enums-impl.md) |
| `Mutex`/`RwLock` 与守卫、锁中毒恢复 | [04 §5.1](04-borrowing-strings-slices.md) |
| trait 对象(`Arc<dyn Trait>`)与函数指针回调 | [06 §4.2](06-generics-traits.md) |
| `move` 闭包、`FnOnce` 与 `write_tx` 写事务 | [07 §4.3](07-iterators-closures.md) |
| `include_str!` 契约追溯门禁(元测试) | [10 §5.1](10-testing.md) |

L3(`src/index/`)引入的 Rust 特性落在以下章节:

| L3 新特性 | 落在哪一节 |
|---|---|
| `TryFrom`、`checked_*`/`saturating_*` 受检运算 | [02 §2.1.2](02-values-and-ownership.md) |
| 浮点 `is_finite`/`clamp`/`MIN_POSITIVE` 防 NaN | [02 §2.2](02-values-and-ownership.md) |
| `Arc<[f32]>` 与 `Vec::into_boxed_slice` | [02 §3.7](02-values-and-ownership.md) |
| 结构体更新语法(`..GRAPH_PARAMS`) | [03 §1.1](03-structs-enums-impl.md) |
| 手写 `PartialEq`/`Eq`/`PartialOrd`/`Ord`、`f32::total_cmp` | [03 §4.2](03-structs-enums-impl.md) |
| `to_le_bytes`/`from_le_bytes`、`copy_from_slice`/`extend_from_slice`、字节串 `b"..."` | [04 §2.3](04-borrowing-strings-slices.md) |
| 借用结构体与生命周期参数(`QueryRef<'a>`) | [04 §4.4](04-borrowing-strings-slices.md) |
| `is_none_or`/`ok_or_else`/`.err()` 组合子 | [05 §1.2](05-errors.md) |
| 引用解构模式、`while let`、`let...else`、let 链 | [05 §4](05-errors.md) |
| `BinaryHeap`/`std::cmp::Reverse`、`HashSet` 访问去重 | [07 §5](07-iterators-closures.md) |
| `pub(crate) use` 受限重导出 | [08 §3](08-modules-docs.md) |
| `proptest` 自定义 `Strategy`/`prop_oneof!`/变异策略 | [10 §4.5](10-testing.md) |
| `thread_local!`+`Cell` 操作计数探针(复杂度验证) | [10 §4.6](10-testing.md) |

L4(`src/query/`)引入的 Rust 特性落在以下章节:

| L4 新特性 | 落在哪一节 |
|---|---|
| 递归枚举与 `Box<Expr>`/`Box<[Expr]>` | [03 §3.4](03-structs-enums-impl.md) |
| 固有 `Expr::from_str` 与 `#[allow(clippy::…)]` | [03 §2.1.2](03-structs-enums-impl.md) |
| 字段初始化简写、`match` 守卫、元组变体 `(..)` 模式 | [03 §1.1](03-structs-enums-impl.md)、[03 §3.3](03-structs-enums-impl.md)、[05 §4.4](05-errors.md) |
| `div_euclid`/`rem_euclid`/`div_ceil`/`unsigned_abs` | [02 §2.1.3](02-values-and-ownership.md) |
| `char` 的 Unicode 判定与 `len_utf8`、`chars().next()` | [02 §2.4](02-values-and-ownership.md) |
| `f64::INFINITY`/`NEG_INFINITY`(融合中间量)、`f32::EPSILON`、`f64` 精确整数上限 | [02 §2.2](02-values-and-ownership.md) |
| `[u8]::strip_prefix`、字节字面量 `b'0'`、`is_ascii_digit` | [04 §2.4](04-borrowing-strings-slices.md) |
| 原始字符串 `r#"..."#`、`str` 模式 API(闭包/字符数组)、`trim_start`、`String::push`/`push_str` | [04 §3](04-borrowing-strings-slices.md)、[04 §3.4](04-borrowing-strings-slices.md) |
| `AsRef`(`.as_ref()` 借出 `&str`)、`Arc<str>` 与 `str` 直接比较 | [04 §3.3](04-borrowing-strings-slices.md) |
| `impl<'a> Parser<'a>`、`fn rest(&self) -> &'a str` | [04 §4.5](04-borrowing-strings-slices.md) |
| 原子类型(`AtomicU64` + `fetch_add`) | [04 §5.2](04-borrowing-strings-slices.md) |
| `then_some`/`is_some_and`/`filter`/`as_deref`/`as_ref`/`cloned`/`copied`/`or_else`/`unwrap_or_default`/`map_or_else` | [05 §1.2](05-errors.md) |
| 默认绑定模式(匹配 `&T` 免写 `&`)、let 链混普通条件 | [05 §4.2](05-errors.md)、[05 §4.7](05-errors.md) |
| `collect` 收成 `Result<Vec<_>>` | [05 §2.4](05-errors.md) |
| 枚举变体构造器转函数指针、`Option::map` 接构造器 | [06 §4.2](06-generics-traits.md) |
| 运算符重载(`std::ops::BitAnd`/`BitOr`)与关联类型 | [06 §5](06-generics-traits.md) |
| `HashMap` entry API、`keys()`/`map[key]` 索引、`sort` + `dedup` | [07 §5.2](07-iterators-closures.md) |
| `Iterator::find_map`、`Vec::extend`/`truncate` | [07 §3](07-iterators-closures.md) |
| `fold` 配 `f64::min`/`f64::max` 函数指针求极值 | [07 §3.3](07-iterators-closures.md) |
| `fmt::Write` 与 `write_char`/`write_str`(trait 须在作用域)、`use` 组里的 `self` | [08 §4](08-modules-docs.md) |
| `pub(super)` 子模块受限方法 | [08 §3](08-modules-docs.md) |
| `pub use` 重导出第三方宏(`mneme::json`) | [08 §1](08-modules-docs.md) |
| 测试替身 `FakeClock`(`AtomicI64`,可注入时钟) | [10 §1.4](10-testing.md) |
| 集成测试共享助手(`tests/common/mod.rs`) | [10 §2](10-testing.md) |
| proptest 字符串正则策略(`".{0,200}"`) | [10 §4](10-testing.md) |
| `format!("{month:02}")` 宽度与补零 | [01 §6](01-toolchain.md) |

L2(`src/persist/`)与 L5(`src/life/`)引入的 Rust 特性落在以下章节:

| L2/L5 新特性 | 落在哪一节 |
|---|---|
| `std::thread::spawn`/`JoinHandle`/`join`、`thread::scope` | [04 §5.3](04-borrowing-strings-slices.md) |
| `Weak`/`Arc::downgrade`/`upgrade`(后台线程不阻止 Drop) | [04 §5.3](04-borrowing-strings-slices.md) |
| `Condvar` 与 `wait_timeout`/`notify_all`(虚假唤醒、丢通知) | [04 §5.3](04-borrowing-strings-slices.md) |
| 原子类型(`AtomicI64`)与 `compare_exchange_weak` CAS 循环(`MonotonicClock`) | [04 §5.3](04-borrowing-strings-slices.md) |
| `Drop`/RAII(最后句柄停线程、`FileLock` 尽力解锁) | [04 §5.4](04-borrowing-strings-slices.md) |
| `std::io`:`File`/`Read`/`Seek`/`read_exact`/`write_all`/`ErrorKind` | [05 §3.5](05-errors.md) |
| `usize::try_from`、`std::mem::size_of`(32 位防截断) | [05 §3.5](05-errors.md) |
| `memmap2::Mmap::map` 的 `unsafe` 与 `// SAFETY:` | [09 §4.2](09-cfg-unsafe-simd.md) |
| `#[cfg]`/`#[cfg_attr]` 条件类型与后端切换 | [01 §4.2](01-toolchain.md)、[09 §4.2](09-cfg-unsafe-simd.md) |
| `#[cfg(feature = "mmap")] pub(crate) struct` 条件模块声明 | [08 §2](08-modules-docs.md) |
| `impl AsRef<Path>` 路径参数惯例 | [06 §2.3](06-generics-traits.md) |
| `PhantomData`(借用型句柄) | [06 §2.3](06-generics-traits.md) |
| 手写 `Debug` + `finish_non_exhaustive()` | [06 §3.2](06-generics-traits.md) |
| `or_default`/`sort_by_key`/`binary_search`/`filter_map`/`windows`/`peekable` | [07 §3.4](07-iterators-closures.md) |
| 跨线程共享的测试状态用原子(`FsyncHook` 实现的 `AtomicUsize` 计数器) | [10 §4.6](10-testing.md) |
| 共享测试助手全貌(`tests/common/mod.rs`) | [10 §2](10-testing.md) |

L6(`src/quant/`、`src/memory/async_facade/`、`benches/quant.rs` 与 `fuzz/`)引入的 Rust 特性落在以下章节:

| L6 新特性 | 落在哪一节 |
|---|---|
| `half::f16` 半精度类型与 `from_f32`/`to_bits`/`from_bits`/`to_f32` 位级 API | [02 §2.3](02-values-and-ownership.md) |
| `usize::is_multiple_of`、`f32::round`/`powi` | [02 §2.1](02-values-and-ownership.md)、[02 §2.2](02-values-and-ownership.md) |
| 枚举 + `match` 的量化格式分派(不为每种格式引入新 trait) | [03 §3.5](03-structs-enums-impl.md) |
| `chunks`/`chunks_exact` 按块处理向量 | [07 §3](07-iterators-closures.md) |
| `#[cfg(feature = "quant-f16")]` 条件模块声明、`#[cfg(test)]`/`#[cfg(not(...))]` 条件导入 | [08 §2](08-modules-docs.md) |
| `cfg!(feature = "quant-f16")` 编译期特性门控(单点定义、不静默降级) | [09 §1.1](09-cfg-unsafe-simd.md) |
| `#[cfg(test)]` 单函数级标注(测试专用编解码 API) | [10 §1.5](10-testing.md) |
| feature 门控测试(`#[cfg(feature)]`/`#[cfg(not(feature))]` 成对) | [10 §1.5](10-testing.md) |
| `async fn`/`.await`/`Future` 的惰性与 `tokio::task::spawn_blocking` 约束 | [11 §1](11-async-tokio.md)、[11 §2](11-async-tokio.md) |
| `block_on`、`no_run` 异步 doctest 与条件重导出 `AsyncNamespace` | [11 §3](11-async-tokio.md)、[08 §1](08-modules-docs.md) |
| future drop 的取消语义、owned 返回跨线程(`StoredRecord`) | [11 §4](11-async-tokio.md)、[11 §5](11-async-tokio.md) |
| `criterion` 基准:`criterion_group!`/`BenchmarkId`/`bench_with_input`/`std::hint::black_box`/`harness = false` | [10 §6](10-testing.md) |
| fuzz 骨架:`#![no_main]`/`fuzz_target!`/独立 workspace/`feature = "fuzzing"` | [10 §7](10-testing.md) |

> L1 的业务语义(命名空间、写事务、去重、双时态等)、L2 的业务语义(段布局、WAL、
> MANIFEST、崩溃恢复)、L3 的业务语义(HNSW 构建/查询、过滤三档、hidx 字节布局)、
> L4 的业务语义(DSL 文法、BM25、RRF 融合、计划器)、L5 的业务语义(compaction、
> 遗忘、命名空间、快照备份)与 L6 的业务语义(量化误差界与 qvec 布局、两阶段候选预算、
> 召回门槛与自动回退、async 门面语义)不属于语言教学,分别见
> [设计 03 L1](../design/03-l1-memory.md)、[设计 04 L2](../design/04-l2-persist.md)、
> [设计 05 L3](../design/05-l3-hnsw.md)、[设计 06 L4](../design/06-l4-query.md)、
> [设计 07 L5](../design/07-l5-life.md) 与 [设计 08 L6](../design/08-l6-quant.md)。

---

## 5. 学完之后的下一步(通用资料)

本套文档只覆盖"读懂 mneme 所需的最小集"。想系统深入,推荐:

- **《The Rust Programming Language》**(官方书,俗称 "the book",中文版《Rust 程序设计语言》):
  <https://doc.rust-lang.org/book/>,中文版见 <https://kaisery.github.io/trpl-zh-cn/>。
- **《Rust by Example》**:<https://doc.rust-lang.org/rust-by-example/>,例子驱动。
- **标准库文档**:<https://doc.rust-lang.org/std/>,查 `Vec`、`Iterator`、`Option` 的方法时最好用。
- **Clippy** lint 列表:<https://rust-lang.github.io/rust-clippy/>,理解"惯用写法"。

> mneme 的编码约束(注释、命名、错误处理、测试)另见仓库根目录的
> [CONTRIBUTING.md](../../CONTRIBUTING.md) 与设计文档
> [DESIGN.md](../DESIGN.md)。

---

## 6. 本书约定

- 代码块若标注 `rust`,大多数可直接编译运行或与源码一致;含 `...` 的是为聚焦重点而省略的片段,
  不能直接编译。标注 `text` 的是编译器报错或输出。
- `文件:行号` 指向编写本套文档时的源码位置,后续重构可能移动行号,但**函数/类型名不变**,
  可按名字搜索。
- 术语首次出现给英文原文,如"所有权(ownership)";完整术语表见
  [设计文档 15 术语表](../design/15-glossary.md)。
- 数学公式用 KaTeX 书写,与设计文档一致。

---

## 下一章

[01 工具链与 Cargo](01-toolchain.md):先把 `cargo build` / `cargo test` 跑起来,
再看 mneme 的 `Cargo.toml` 每一行在说什么。
