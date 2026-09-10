# Mneme 的 Rust 零基础教学

> **本套文档写给谁**:完全没写过 Rust、但会用至少一门其他语言(如 Python/Java/C/Go/JavaScript)
> 写代码的人。你不需要任何 Rust 基础,也不需要 AI 或数据库背景。
>
> **本套文档想解决什么**:让你能**独立读懂 `mneme` 的源码**。本套以 L0 原语层
> `src/core/` 为教材,并把 L1 内存引擎 `src/memory/` 新引入的 Rust 知识
> (`Arc` 共享所有权、写时复制、`Mutex`/`RwLock` 守卫、`move` 闭包写事务、
> `dyn` 策略与函数指针、构建者模式)**回填到各章对应小节**(见 §4 对照表);
> L1 的业务语义与分文件阅读路线见 [设计 03 L1 内存引擎](../design/03-l1-memory.md)。
> 所有语法点都锚定在 mneme 的真实代码上,不讲"为了教语法而教语法"的空例子。
>
> **预计阅读**:6–10 小时(边读边敲会更快掌握)。建议**开着源码对照阅读**。

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

- **顺序读**:01 → 10。每章只依赖前面章节,不跳步。
- **边读边跑**:每章末尾有「动手练习」,在 `examples/` 下新建一个文件敲一遍(或用临时 crate)。
  **不要直接改 `src/core/` 与 `src/memory/`**:库里的公开项受 `#![deny(missing_docs)]` 约束,乱加还会污染源码。
  光看不敲,Rust 的所有权和借用是学不会的。
- **对照源码**:遇到 `文件:行号` 就跳过去看完整上下文。
- **不要背语法**:Rust 编译器报错信息极其友好,学会"看报错 → 改代码"比背规则更重要。
  每章都列了「你会遇到的编译器报错」。

> 阅读前请先确认本机已装 Rust 工具链;没有的话见 [01 工具链](01-toolchain.md) 的安装小节。

---

## 3. 章节地图

| 章 | 标题 | 一句话 | 读完后你能看懂 mneme 里的…… |
|---|---|---|---|
| [01](01-toolchain.md) | 工具链与 Cargo | `cargo` 怎么构建、测试、出文档 | `Cargo.toml`、edition、MSRV、`cargo test/doc` |
| [02](02-values-and-ownership.md) | 值、类型与所有权 | Rust 最独特、也最容易卡住的部分 | `u32`/`f32`、`mut`、移动/复制/克隆、newtype、`Arc`/写时复制 |
| [03](03-structs-enums-impl.md) | 结构体、枚举与 impl | 用类型描述数据、用 `impl` 挂行为 | `RowId`、`Metric`、`MnemeError`、`#[derive]`、`Builder` 链 |
| [04](04-borrowing-strings-slices.md) | 引用、借用、生命周期与字符串 | `&`、`&mut`、`&str`/`String`/`Arc<str>` | `dot(a, b)`、`Key`、`get_path` 的 `'v`、锁守卫 |
| [05](05-errors.md) | 错误处理 | `Option`/`Result`/`?`/`thiserror` | `MnemeError`、`Result<T>`、varint 的畸形输入 |
| [06](06-generics-traits.md) | 泛型与 trait | 一套代码适配多种类型 | `TopK<T: Ord>`、`Clock`、`From`/`Display`、`dyn` 策略 |
| [07](07-iterators-closures.md) | 迭代器与闭包 | 用链式调用替代手写循环 | `dot_scalar`、`TopK::into_sorted_vec`、`write_tx` 闭包 |
| [08](08-modules-docs.md) | 模块、可见性与文档 | 代码怎么分文件、怎么暴露 | `lib.rs`、`core/mod.rs`、`//!` 与 `///` |
| [09](09-cfg-unsafe-simd.md) | 条件编译、unsafe 与 SIMD | 跨平台与手写向量指令 | `simd.rs` 的 `#[cfg(target_arch)]`、`unsafe` |
| [10](10-testing.md) | 测试与属性测试 | `#[test]`、doctest、`proptest` | `tests/core_contracts.rs`、契约测试 |

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
| `src/core/simd.rs` | `unsafe`、`#[cfg]`、`#[target_feature]`、切片、迭代器 | [04](04-borrowing-strings-slices.md)、[07](07-iterators-closures.md)、[09](09-cfg-unsafe-simd.md) |
| `src/core/heap.rs` | 泛型 + trait bound、`Vec`、闭包、`Ordering`、`Option` | [02](02-values-and-ownership.md)、[06](06-generics-traits.md)、[07](07-iterators-closures.md) |
| `src/core/varint.rs` | 位运算、`Result`、`?`、迭代器、边界校验 | [02](02-values-and-ownership.md)、[05](05-errors.md)、[07](07-iterators-closures.md) |
| `src/core/meta.rs` | 类型别名、生命周期、`Option` 链、`serde_json` | [04](04-borrowing-strings-slices.md)、[05](05-errors.md) |
| `src/core/options/*.rs` | `struct`、`enum`、`Default`、`#[default]`、嵌套 `Option` | [03](03-structs-enums-impl.md)、[06](06-generics-traits.md) |
| `src/core/mod.rs` | 模块组织、`//!` 模块文档 | [08](08-modules-docs.md) |
| `tests/core_contracts.rs` | 集成测试、`proptest`、契约追溯 | [10](10-testing.md) |

L1(`src/memory/`)新引入的 Rust 特性已**回填到对应章节**,不在上表重复:

| L1 新特性 | 落在哪一节 |
|---|---|
| `Arc<T>` 共享所有权、`Arc::make_mut` 写时复制 | [02 §3.7](02-values-and-ownership.md) |
| 链式构建者模式(`mut self -> Self`) | [03 §2.2.3](03-structs-enums-impl.md) |
| `Mutex`/`RwLock` 与守卫、锁中毒恢复 | [04 §5.1](04-borrowing-strings-slices.md) |
| trait 对象(`Arc<dyn Trait>`)与函数指针回调 | [06 §4.2](06-generics-traits.md) |
| `move` 闭包、`FnOnce` 与 `write_tx` 写事务 | [07 §4.3](07-iterators-closures.md) |
| `include_str!` 契约追溯门禁(元测试) | [10 §5.1](10-testing.md) |

> L1 的业务语义(命名空间、写事务、去重、双时态等)不属于语言教学,见
> [设计 03 L1 内存引擎](../design/03-l1-memory.md)。

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
