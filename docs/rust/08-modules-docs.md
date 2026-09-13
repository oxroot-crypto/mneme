# 08 模块、可见性与文档

> **本章目标**:理解 `mod` / `pub` / `pub use` / `use` 如何组织代码与暴露 API,
> 以及 `//!` 模块文档、`///` 条目文档和 doctest 怎么写。
> **前置**:[03 章](03-structs-enums-impl.md)。
> **对应源码**:[`src/lib.rs`](../../src/lib.rs)、[`src/core/mod.rs`](../../src/core/mod.rs)、
> [`src/core/options/mod.rs`](../../src/core/options/mod.rs)、[`src/core/metric.rs`](../../src/core/metric.rs)、
> [`src/index/mod.rs`](../../src/index/mod.rs)、[`src/query/parse/`](../../src/query/parse)、
> [`src/query/display.rs`](../../src/query/display.rs)。

Rust 用**模块(module)**组织命名空间,用**可见性(visibility)**控制谁能访问。
mneme 的模块划分直接对应架构分层,读模块结构就能读出设计。

---

## 1. crate 根:`src/lib.rs`

`src/lib.rs` 是库 crate 的根。mneme 的它做了三件事:

```rust
//! Mneme:面向 AI Agent 超长期记忆层的嵌入型向量存储引擎。
//! ...(模块文档)

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod core;
#[cfg(feature = "fuzzing")]
pub mod fuzzing;
pub mod memory;

mod index;
mod life;
mod persist;
mod quant;
mod query;

pub use crate::core::error::{MnemeError, Result};
pub use crate::core::heap::TopK;
pub use crate::core::meta;
...
```

见 [`src/lib.rs`](../../src/lib.rs)。

- `//!` 是**内部文档注释**,写在文件/模块开头,描述这个 crate。
- `#![...]` 是**crate 级属性**(注意 `#!` 而非 `#`):
  - `#![deny(missing_docs)]`:任何公开项缺文档就**编译失败**;
  - `#![deny(unsafe_op_in_unsafe_fn)]`:在 `unsafe fn` 里做不安全操作必须再包一层 `unsafe` 块,
    强制显式(见 [09 章](09-cfg-unsafe-simd.md))。
- `pub mod core;` 声明一个公开子模块;`mod index;`/`mod quant;` 等不带 `pub` 的是
  crate 内部模块(`quant` 是 L6 纯原语模块,依赖等级同 L0,见 [AGENTS.md](../../AGENTS.md))。
- `#[cfg(feature = "fuzzing")] pub mod fuzzing;` 说明**模块声明也能条件编译**:只有打开
  `fuzzing` feature(即 `fuzz/` 构建)时,`src/fuzzing.rs` 才参与编译,普通运行时不暴露
  (见 [10 §7](10-testing.md))。
- `pub use ...` 是**重导出(re-export)**:把深层路径的项提升到 crate 根,
  让用户能写 `use mneme::Metric;` 而不是 `use mneme::core::metric::Metric;`。
  重导出同样能带条件:L6 的 `AsyncNamespace` 写成
  `#[cfg(feature = "async")] pub use crate::memory::AsyncNamespace;`
  (见 [`src/lib.rs:67-68`](../../src/lib.rs))。
- `pub use` 也能重导出**宏**:`core/meta.rs` 里的 `pub use serde_json::json;` 把第三方
  `json!` 宏转成本 crate 的 `mneme::json`。L4 的 doctest 因此写 `use mneme::{Expr, json};`,
  库用户不必直接依赖 serde_json(见 [`src/core/meta.rs:10`](../../src/core/meta.rs))。

---

## 2. 模块与文件

一个模块可以:

- 写成单独文件 `foo.rs`;
- 或写成目录 `foo/` + `foo/mod.rs`(旧风格)或 `foo.rs` + `foo/`(新风格,`foo.rs` 作为目录入口)。

mneme 的 `core` 模块用目录:

```text
src/
  lib.rs          # crate 根:pub mod core/memory;私有 mod index/life/persist/quant/query
  core/           # L0 原语层(公开)
    mod.rs        # 声明子模块
    types.rs error.rs metric.rs simd.rs heap.rs varint.rs meta.rs bitset.rs text.rs
    options/
      mod.rs
      clock.rs dimension.rs ...
  memory/         # L1 内存引擎(公开)
    mod.rs engine.rs engine_ops.rs config.rs record.rs search.rs pred.rs ...
    table/ namespace/ analysis/ ...
  persist/        # L2 持久层(crate 内部;门面经 memory 暴露)
    mod.rs vsec.rs manifest.rs edges.rs flush.rs source.rs storage.rs
    codec.rs hook.rs trash.rs
    wal/ msec/ recover/ store/
  index/          # L3 索引层(crate 内部)
    mod.rs hnsw.rs graph.rs filtered.rs rebuild.rs hidx.rs factory.rs
  query/          # L4 检索层(crate 内部)
    mod.rs parse/ display.rs json.rs iso.rs plan.rs zmap.rs bm25.rs fusion.rs exec.rs
  life/           # L5 生命周期层(crate 内部)
    mod.rs compact.rs maintenance.rs
  quant/          # L6 量化原语(crate 内部;纯原语,依赖等级同 L0)
    mod.rs f16.rs scalar_i8.rs rescore.rs support.rs
  fuzzing.rs      # fuzz 专用解析入口(feature "fuzzing";仅 fuzz 构建)
```

`src/core/mod.rs` 只做模块声明与文档:

```rust
//! L0 原语层:类型、错误、距离数学与基础算法。
//! ...
pub(crate) mod bitset;
pub mod error;
pub mod heap;
pub mod meta;
pub mod metric;
pub mod options;
pub mod simd;
pub mod text;
pub mod types;
pub mod varint;
```

> `bitset` 是 `pub(crate)`(见下节):它只服务 crate 内部的索引与计划器,不对外暴露。

见 [`src/core/mod.rs`](../../src/core/mod.rs)。**规范要求 `mod.rs` 只做组织与 `pub use`,不写业务逻辑。**

> **条件模块/条件项**:`mod` 声明与其他项一样能用 `#[cfg]` 控制。L2 的 `source.rs` 里
> `MmapSource` 整个类型只在 `feature = "mmap"` 时存在:
>
> ```rust
> #[cfg(feature = "mmap")]
> pub(crate) struct MmapSource {
>     map: memmap2::Mmap,
> }
> ```
>
> 见 [`src/persist/source.rs:89-92`](../../src/persist/source.rs)。关闭 feature 时该类型
> 与相关方法都不参与编译,由 `FileSource` 兜底(见 [09 §4.2](09-cfg-unsafe-simd.md))。

条件还能加在**模块声明**上。L6 的 `quant` 模块把 f16 子模块整个挂上 feature 门:

```rust
pub(crate) mod rescore;
pub(crate) mod scalar_i8;
mod support;

#[cfg(feature = "quant-f16")]
pub(crate) mod f16;
```

见 [`src/quant/mod.rs:7-12`](../../src/quant/mod.rs)。关闭 `quant-f16` 时 `f16.rs` 根本不参与
编译,`persist`/`index` 里对它的调用各自带门控(见 [09 §1.1](09-cfg-unsafe-simd.md) 的
`cfg!` 单点门控)。`use` 也能带条件,测试模块里常用**互补条件**成对引入:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(feature = "quant-f16"))]
    use crate::core::error::MnemeError;
    use crate::core::options::VectorFormat;
    ...
}
```

见 [`src/quant/mod.rs:16-21`](../../src/quant/mod.rs)。`#[cfg(not(feature = "quant-f16"))] use ...`
只在关闭 feature 时引入 `MnemeError`,这样两种构建下都不会出现"未使用 import"告警;
单测文件里也有 `#[cfg(test)] use ...`,只给测试代码引入辅助类型
(见 [`src/quant/f16.rs:9-10`](../../src/quant/f16.rs))。

---

## 3. 可见性:默认私有

- 不带 `pub` 的项**仅在本模块及其子模块可见**。
- `pub` 表示对所有能访问到该模块的地方可见。
- `pub(crate)` 表示**仅在本 crate 内可见**,不对外暴露。
- `pub(super)` 仅父模块可见。

L4 的 `parse/literal.rs` 是 `parse` 的子模块,父模块要调用里面的取值解析方法,于是标为
`pub(super)`——只对父模块开放:

```rust
impl<'a> Parser<'a> {
    pub(super) fn parse_value(&mut self) -> Result<Val> { ... }
}
```

见 [`src/query/parse/literal.rs:38-39`](../../src/query/parse/literal.rs)。同级模块之间**看不到**
对方的私有项;要跨兄弟模块共享,只能放宽到 `pub(super)`(父模块)或 `pub(crate)`(全 crate)。

mneme 的 `options/mod.rs` 是很好的例子:

```rust
mod clock;         // 私有子模块
mod dimension;
...

pub use clock::{Clock, SystemClock};   // 但把类型重导出为公开
pub use dimension::Dimension;
...
```

见 [`src/core/options/mod.rs:16-30`](../../src/core/options/mod.rs)。

**设计意图**:内部按主题拆成小文件(满足单一职责),但对外只暴露统一的类型路径。
用户可以 `use mneme::Clock;`,而 `options::clock` 这个路径是隐藏的——将来重构文件结构不会破坏用户。

> **注意**:`#![deny(missing_docs)]` 只要求**公开**项有文档;私有项和 `pub(crate)` 项不强制
> (但 mneme 规范仍要求都写)。把内部类型设为 `pub(crate)` 而不是 `pub`,也能避免把实现细节
> 写进公开 API 文档。

重导出也能带可见性修饰。L3 的 `index/mod.rs` 把索引工厂定为 `pub(crate)`,只给组合根使用:

```rust
mod factory;

pub(crate) use factory::default_factory;
```

见 [`src/index/mod.rs:27`](../../src/index/mod.rs)。`pub(crate) use` 是"仅在本 crate 内重导出":
门面(`memory::Builder`)能用,下游用户看不到——这正是 [AGENTS.md](../../AGENTS.md) 里
"门面可注入 `IndexFactory`"的可见性边界。

---

## 4. `use` 导入

```rust
use std::fmt;
use std::sync::Arc;

use crate::core::metric::Metric;
use crate::core::types::{Key, SegmentId};
```

见 [`src/core/types.rs:9-10`](../../src/core/types.rs) 与 [`src/core/error.rs:9-10`](../../src/core/error.rs)。

导入顺序规范(组间空行):

1. `std` / `core` / `alloc`
2. 外部 crate(如 `serde_json`)
3. `crate::`
4. `super::` / `self::`

路径前缀:

| 前缀 | 含义 |
|---|---|
| `crate::` | 从当前 crate 根开始 |
| `super::` | 父模块 |
| `self::` | 当前模块 |
| `std::` | 标准库 |

mneme 规范优先 `crate::` 绝对路径,避免 `../../..` 这种脆弱写法。

**一个容易踩的坑:trait 方法要先引入 trait 才能调用。** L4 的 `display.rs` 要往
`Formatter` 里写字符,必须把 `std::fmt::Write` 带进作用域:

```rust
use std::fmt::{self, Write};

f.write_char('(')?;      // 来自 fmt::Write trait
f.write_str("never")?;   // 同上
```

见 [`src/query/display.rs:9`](../../src/query/display.rs)。否则编译器只会报
`no method named write_char found for ...`——**错误信息里不会提示你缺 `use`**。
这条规则对所有 trait 方法都成立:方法定义在 trait 上,不是类型本身(见
[06 §2](06-generics-traits.md));`std::io::Read`/`Write`、`Iterator` 等同理
(`Iterator` 在 prelude 里,所以平时感觉不到)。

`{self, Write}` 里的 `self` 指"这个模块本身"——这一行同时把模块名 `fmt` 与 trait `Write`
带进作用域(`fmt::Formatter`、`fmt::Result` 因此可用)。`use` 的路径组里都能写 `self`:
L4 的 plan.rs 用 `use crate::memory::pred::{self, EvalCtx, Expr};` 同时引入模块 `pred`
(以便调用 `pred::matches`)与两个类型,见 [`src/query/plan.rs:10`](../../src/query/plan.rs)。

---

## 5. 文档:rustdoc

Rust 的文档注释会被 `cargo doc` 渲染成 HTML,也是 doctest 的来源。

### 5.1 模块文档 `//!`

放在文件顶部,描述模块职责与边界:

```rust
//! L0 距离度量。
//!
//! 三种度量 [`Metric::Cosine`] / [`Metric::Dot`] / [`Metric::Euclidean`] 统一归结为
//! **一次点积**:范数列存储的是**范数平方**……
//!
//! 方向由 [`Metric::better`] 统一——所有 TopK、归并与排序都必须经 `better()` 比较,
//! **绝不直接比较 `score` 的数值大小**。
```

见 [`src/core/metric.rs:1-11`](../../src/core/metric.rs)。

### 5.2 条目文档 `///`

放在函数/类型/字段上方。mneme 规范要求导出成员**按需**包含以下段落(没有就省略):

| 段落 | 内容 |
|---|---|
| 首行 | 一句话功能描述 |
| `# Arguments` | 每个参数一行,写明含义、单位、约束 |
| `# Returns` | 返回值语义;`None`/`Err` 的触发条件 |
| `# Errors` | `Result` 返回时的错误变体及触发条件 |
| `# Panics` | 可能 panic 的条件 |
| `# Safety` | `unsafe fn` 的前置条件 |
| `# Examples` | 可运行的 doctest |

完整示例见 [`src/core/metric.rs:33-56`](../../src/core/metric.rs):

```rust
/// 计算原始分数。
///
/// # Arguments
///
/// * `a`、`b` - 两个等长向量。
/// * `a_norm`、`b_norm` - 两向量的**范数平方**(`‖a‖²`、`‖b‖²`)。仅
///   [`Metric::Dot`] 不需要,可传 `0.0`。
///
/// # Returns
///
/// `Dot` 返回 `a·b`;`Cosine` 返回 `a·b / sqrt(a_norm * b_norm)`,分母低于
/// `1e-12` 时返回 `0`;`Euclidean` 返回 `a_norm + b_norm - 2 * a·b`。
///
/// # Panics
///
/// 当 `a` 与 `b` 长度不等时,在 debug 构建下 panic;release 构建下按较短者计算。
///
/// # Examples
///
/// ```
/// use mneme::Metric;
///
/// assert_eq!(Metric::Dot.score(&[1.0, 2.0], &[3.0, 4.0], 0.0, 0.0), 11.0);
/// ```
```

### 5.3 doctest:文档里的代码会被测试

````rust
/// # Examples
///
/// ```
/// use mneme::Metric;
/// assert_eq!(Metric::Dot.score(&[1.0, 2.0], &[3.0, 4.0], 0.0, 0.0), 11.0);
/// ```
````

- `cargo test` 会**提取并运行**所有 ``` 代码块。
- 因此文档示例**永远不会过期**——示例错,测试就红。
- 这是 mneme "文档与实现同步"的硬保障。

### 5.3.1 doctest 的隐藏开关

代码块开头的语言标记后可以加修饰词,控制 `cargo test` 的行为:

| 语言标记 | 行为 |
|---|---|
| `rust` | 正常编译并运行 |
| `rust,no_run` | 只编译,不运行(适合写文件/联网的示例) |
| `rust,ignore` | 编译和运行都跳过(尽量避免,示例会腐烂) |
| `rust,should_panic` | 运行且必须 panic,否则测试失败 |
| `rust,compile_fail` | 必须编译失败(用来演示"这样写会报错") |

两个容易忽略的细节:

- 每个 doctest 会被自动包进一个隐式的 `fn main() { ... }`,示例里不用自己写 `main`。
- doctest 支持 `?`:当示例返回 `Result` 时可直接用 `?`,但最后要有 `Ok(())`(可用行首 `#` 把它藏起来)。
- 行首的 `#` 把样板代码从渲染结果里隐藏,但仍参与编译。

### 5.4 文档内链接

```rust
/// 见 [`Metric::better`] 与 [`MnemeError::Corrupted`](crate::core::error::MnemeError::Corrupted)。
```

方括号里的路径会被 rustdoc 变成可点击链接。mneme 文档里大量使用。

---

## 6. 你会遇到的编译器报错

| 报错关键词 | 原因 | 修法 |
|---|---|---|
| `missing documentation for ...` | 公开项没写 `///` | 补文档,或设为私有 |
| `unresolved import` | 路径写错 | 检查 `crate::`/`super::` 与模块是否 `pub` |
| `private ... in public interface` | 公开函数用了私有类型 | 把类型也设为 `pub` |
| `unused import` | 导入了没用 | 删除 |
| `cannot find function ... in module` | 模块没 `pub` 或没 `mod` 声明 | 检查声明与可见性 |

---

## 7. 本章小结

- `src/lib.rs` 是 crate 根,声明模块、重导出公共 API、写 crate 级属性。
- 模块用 `mod` 声明,可拆成文件/目录;`mod.rs` 只做组织与 `pub use`。
- `mod`/`use`/`pub use` 都能用 `#[cfg(...)]` 条件化(L6 的 `f16` 子模块、`fuzzing` 入口与
  `AsyncNamespace` 重导出),测试里常用互补条件成对声明,避免"未使用 import"告警。
- 默认私有;`pub` / `pub(crate)` / `pub(super)` 逐级放开;重导出同样能带可见性(`pub(crate) use`)。
- trait 方法要先 `use` 对应 trait 才能调用,如 `std::fmt::Write` 的 `write_char`/`write_str`。
- `pub use` 重导出让用户只依赖稳定路径,文件结构可自由重构。
- `//!` 模块文档 + `///` 条目文档 + doctest 让文档可渲染、可测试、不过期;
  `#![deny(missing_docs)]` 强制公开项 100% 有文档。

## 动手练习

1. 在 `examples/` 下新建一个模块文件 `greet.rs`,在 `hello.rs` 里 `mod greet;` 并调用其中的 `pub fn`。
2. 给 [03 章练习](03-structs-enums-impl.md)的 `Shape` 每个变体和 `area` 方法加上 `///` 文档。
3. 在 `area` 的文档里写一个 doctest 并运行 `cargo test` 确认它通过。

## 下一章

[09 条件编译、unsafe 与 SIMD](09-cfg-unsafe-simd.md):跨平台代码与手写向量指令。
