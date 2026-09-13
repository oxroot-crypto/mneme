# 01 工具链与 Cargo:先把项目跑起来

> **本章目标**:装好 Rust,学会用 `cargo` 构建/测试/出文档,并读懂 mneme 的 `Cargo.toml`。
> **前置**:会命令行;不需要任何 Rust 语法。
> **对应源码**:[`Cargo.toml`](../../Cargo.toml)、[`Cargo.lock`](../../Cargo.lock)、[`src/lib.rs`](../../src/lib.rs)。

---

## 1. Rust 是什么:三句话

- **编译型**:源码先编译成机器码再运行,没有 Python 那样的解释器。所以你会看到
  "编译 → 运行"两步,而不是"直接运行"。
- **静态类型 + 强类型**:每个变量、每个函数的类型在编译期就确定;类型不对**编译不过**,
  不会等到运行时才炸。
- **无垃圾回收(no GC)的内存安全**:不靠 GC,也不用手动 `free`;靠**所有权(ownership)**
  在编译期保证不出现野指针、双重释放、数据竞争。这是 Rust 最大的卖点,也是最陡的学习曲线
  (第 [02](02-values-and-ownership.md) 章专讲)。

mneme 是一个**库(library)**,不是一个可执行程序——它被别的程序 `cargo add mneme` 引入后使用。
所以它没有 `main()` 作为入口,入口是 `src/lib.rs`。

---

## 2. 安装工具链:rustup + cargo

Rust 的官方安装器叫 **rustup**,它同时带来编译器 `rustc` 和包管理/构建工具 `cargo`。

1. 打开 <https://rustup.rs/>,按提示安装(Windows 下载 `rustup-init.exe` 并运行)。
2. 安装完成后新开一个终端,验证:

```text
$ rustc --version
rustc 1.95.0 (stable)

$ cargo --version
cargo 1.95.0
```

> 版本号仅为示意;以 `rustc --version` 的真实输出为准。

> mneme 要求 Rust 版本 ≥ `1.93`(见 `Cargo.toml` 的 `rust-version`,§4.3)。
> 如果你的版本号过低就运行 `rustup update stable`。

**rustup 管理的三个概念**:

| 名词 | 含义 |
|---|---|
| toolchain | 一整套配套工具(`rustc`、`cargo`、标准库等),用**通道**(`stable`、`nightly`、`beta`)或**具体版本号**(`1.93.0`)标识 |
| target | 编译目标平台,如 `x86_64-pc-windows-msvc`、`aarch64-apple-darwin` |
| component | 装入某个 toolchain 的可选附加工具,如 `clippy`(静态检查)、`rustfmt`(格式化) |

本项目只用 **stable**,不需要 nightly(见第 [09](09-cfg-unsafe-simd.md) 章的 SIMD 用的是稳定特性)。

---

## 3. crate、package、module:三个容易混的词

| 词 | 含义 | mneme 里的例子 |
|---|---|---|
| **package(包)** | 一个 `Cargo.toml` 定义的项目 | mneme 整个仓库 |
| **crate(编译单元)** | 一次编译的最小单位;一个 package 可以含多个 crate | `src/lib.rs` 定义的那个库 crate |
| **module(模块)** | crate 内部的命名空间,用 `mod` 组织 | `src/core/` 下的 `types`、`error`…… |

- 一个 package 可以有 **1 个库 crate**(`src/lib.rs`)和**多个可执行 crate**(`src/main.rs`
  或 `src/bin/*.rs`)。mneme 只有库 crate。
- `tests/` 目录下的每个文件都是**独立的集成测试 crate**,它们只能通过公开 API 访问库
  (见第 [10](10-testing.md) 章)。
- 模块的语法(怎么分文件、怎么 `pub`)在 [08 章](08-modules-docs.md) 讲。

**依赖的方向**:`tests/core_contracts.rs` → `mneme` 库 → `serde`/`crc32fast` 等外部 crate。
下层不知道上层存在。

---

## 4. 逐段读懂 `Cargo.toml`

mneme 的 `Cargo.toml` 不长,但几乎每一行都值得解释。下面按实际内容分段。

### 4.1 `[package]`:我是谁

```toml
[package]
name = "mneme"                     # crate 名;别人 `use mneme::...` 用的就是它
version = "0.1.0"                  # 语义化版本 semver:主.次.补丁
edition = "2024"                   # Rust 语言版本(见 4.3)
rust-version = "1.93"              # MSRV,最低支持的 Rust 版本(见 4.3)
description = "嵌入型向量存储引擎,为 AI Agent 的超长期记忆层设计"
license = "Unlicense"
repository = "https://gitlab.oxroot.io/rustlib/mneme"
readme = "README.md"
keywords = ["vector", "embedding", "database", "hnsw", "memory"]
categories = ["database"]
```

- `name` 决定代码里 `use mneme::...` 的路径。**package 名和 crate 名通常同名**,但 package
  名里的连字符在 crate 名里会转成下划线(`my-lib` 的 crate 名是 `my_lib`)。
- `version` 遵循语义化版本:[semver.org](https://semver.org/lang/zh-CN/)。
  `0.x` 表示尚未稳定,`0.1.0 → 0.2.0` 允许破坏性变更。
- `description` / `keywords` / `categories` 是发布到 crates.io 时的元数据。

### 4.2 `[features]` 与依赖

先看编译开关,再看依赖。`[features]` 定义**可选的编译分支**:

```toml
[features]
# L3 起:默认开启 mmap;关闭后段文件走 FileSource 兜底。
default = ["mmap"]
# 打开本 feature 时才引入可选依赖 memmap2。
mmap = ["dep:memmap2"]
# L6 f16 量化副本:引入 `half`;关闭时 `VectorFormat::F16` 于构造期返回 `Unsupported`。
quant-f16 = ["dep:half"]
# L6 async 门面:引入 `tokio` 的阻塞线程池;核心库零 tokio。
async = ["dep:tokio"]
# fuzz 专用解析入口:只服务 `fuzz/` 构建,不改变运行时行为。
fuzzing = []
```

- `default = ["mmap"]` 表示不额外指定时也启用 `mmap`;`cargo build --no-default-features`
  则关掉它。代码里用 `#[cfg(feature = "mmap")]` 选择分支(见 [09 章](09-cfg-unsafe-simd.md))。
- `mmap = ["dep:memmap2"]` 里的 `dep:` 前缀表示"只引入这个**可选依赖**,不额外造一个同名
  feature"。依赖在 `Cargo.toml` 里写 `optional = true` 才会变成可选。
- 除了"编不编"的 `#[cfg(...)]`,还有"条件成立时附加属性"的 `#[cfg_attr(条件, 属性)]`。
  例如 `#[cfg_attr(feature = "mmap", allow(dead_code))]` 在开启 mmap 时对那段代码放行
  `dead_code` 告警(此时兜底后端只被测试用到),关闭时则不放行(见
  [`src/persist/source.rs:27-41`](../../src/persist/source.rs))。

再看依赖清单:

```toml
[dependencies]
# 元数据与 DSL 序列化的基础;直接声明以对齐依赖白名单( docs/design/01-overview.md §5 )。
serde = "1"
serde_json = "1"
thiserror = "2"
# L2 持久层的 CRC-32 校验。
crc32fast = "1"
# L3 索引层的 mmap 零拷贝读;可选,由 feature `mmap` 控制。
memmap2 = { version = "0.9", optional = true }
# L6 f16 量化副本的 IEEE 754 half 编解码;可选,由 feature `quant-f16` 控制。
half = { version = "2", optional = true }
# L6 async 门面的 `spawn_blocking`;可选,由 feature `async` 控制;只开 `rt`。
tokio = { version = "1", optional = true, default-features = false, features = ["rt"] }

[dev-dependencies]
proptest = "1"
tempfile = "3"
criterion = { version = "0.8", default-features = false }

[[bench]]
name = "hnsw"
harness = false

[[bench]]
name = "quant"
harness = false
```

| 段 | 作用 | 何时编译 |
|---|---|---|
| `[dependencies]` | 运行库本身需要的 crate | `cargo build` 与 `cargo test` 都编 |
| `[dev-dependencies]` | 仅测试/示例/基准需要的 crate | 只有 `cargo test` 等才编 |
| `[features]` | 编译开关,决定引入哪些可选依赖/代码 | 由命令行或上游依赖选择 |

- 版本写法 `"1"` 是 **caret 语义**:等价于 `"^1"`,表示"任何 `1.x.y`",但**不允许 `2.0.0`**。
  mneme 规范禁止 `"*"` 这种无界版本;`"0.9"` 同理表示 `>=0.9.0, <0.10.0`。
- `optional = true` 的依赖必须被某个 feature 用 `dep:` 引入,否则永远不会参与编译。
- `criterion` 用了 `default-features = false`:关掉它自带的绘图等默认功能;
  `black_box` 自 0.6 起改由 `std::hint` 提供,bench 不再从 criterion 引入。下方
  两个 `[[bench]]` 声明基准目标 `benches/hnsw.rs` 与 `benches/quant.rs`,并关掉
  libtest 的默认 `harness`——基准由 criterion 自己驱动。
- 新增依赖前必须论证(见 [CONTRIBUTING.md](../../CONTRIBUTING.md)):体积、编译时间、
  维护活跃度、能否零依赖自研。mneme 的复杂算法(HNSW、BM25、量化)全部自研,
  依赖白名单上限 4 个(不含 feature 引入的 `memmap2`、`half`、`tokio`)。
- `thiserror` 是**过程宏(proc-macro)** crate,用来给错误枚举自动生成样板代码,见 [05 章](05-errors.md)。

### 4.3 edition 与 MSRV:两个"版本"

- `edition = "2024"`:Rust 每三年发布一个新 **edition**(2015/2018/2021/2024)。
  edition 是**语法兼容性开关**,不是编译器版本;同一个编译器可编译不同 edition 的代码。
  `2024` 是当前最新 edition。
- `rust-version = "1.93"`:MSRV(Minimum Supported Rust Version,最低支持版本)。
  它告诉 cargo 和用户"低于 1.93 的编译器别用"。mneme 的策略是"最新稳定版 − 2"。

> 这行必须**真实**:文档里声明支持什么,CI 就必须测什么。

> **三个"版本"别混**:① `rustc 1.95.0` 是**编译器版本**(你装了哪个工具链);② `edition = "2024"`
> 是**语法/语义开关**(决定数组 `into_iter`、`gen` 关键字等行为),同一个编译器能编译任意 edition 的
> 代码;③ `rust-version = "1.93"` 是 **MSRV**(依赖约束),只告诉 cargo"低于 1.93 别解析这个包"。
> 三者互不替代:MSRV 高不代表你只能写 2024 edition,edition 新也不代表低版本编译器能编。

### 4.4 `[profile.release]`:发布构建怎么优化

```toml
[profile.release]
# L0 为纯函数库,lto 收益有限;thin LTO 在跨 crate 内联与编译时间间折中,
# L2 持久层落地后按 bench 数据再调。
lto = "thin"
```

- Rust 有两种常用构建模式:**debug**(`cargo build`,快、可调试、慢)**和 release**
  (`cargo build --release`,慢、优化、快)。
- `lto`(Link Time Optimization,链接期优化)让编译器跨 crate 边界内联。
  `"thin"` 是折中档:比 `false` 强、比 `"fat"` 快。
- 注意源码注释的写法:**解释"为什么是这个值"**,而不是复述 `lto = "thin"` 本身。
  这是本项目注释规范的一部分。

### 4.5 `Cargo.lock`:锁定精确版本

- `Cargo.toml` 写的是**范围**(`"1"`),`Cargo.lock` 记录实际用的**精确版本**。
- 库项目通常也提交 `Cargo.lock`(mneme 仓库里就有),以保证团队与 CI 构建可复现。
- 不要手改 `Cargo.lock`;改 `Cargo.toml` 后由 cargo 自动更新。

---

## 5. 每天都要用的 cargo 命令

在仓库根目录(含 `Cargo.toml` 的目录)执行:

```bash
cargo build                 # 编译 debug 版
cargo build --release       # 编译 release 版
cargo test                  # 编译并运行所有测试(单测 + 集成测试 + doctest)
cargo test some_test_name   # 只跑名字匹配的测试
cargo run                   # 运行可执行 crate(mneme 是库,没有这个)
cargo doc --no-deps --open  # 生成并打开 rustdoc 文档
cargo clippy --all-targets --all-features -- -D warnings  # 静态检查,警告当错误
cargo fmt                   # 自动格式化代码
cargo fmt --check           # 只检查,不改文件(CI 用)
```

对照 mneme 的检查清单([CONTRIBUTING.md](../../CONTRIBUTING.md)):

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo doc --no-deps
```

> `cargo doc` 会把源码里的 `///` 注释和 `//!` 模块注释渲染成网页。
> mneme 在 `src/lib.rs` 里写了 `#![deny(missing_docs)]`:
> **任何一个公开项没有文档,编译直接失败**。文档的重要性可见一斑(见 [08 章](08-modules-docs.md))。

### 5.1 报错怎么看

Rust 的报错通常会给出:

1. 出错的**位置**;
2. 出错原因;
3. 一个**修改建议**,有时直接给出可复制的代码;
4. 有时还有 `help` / `note`。

例如漏写分号:

```text
error: expected `;`, found `}`
 --> src/lib.rs:3:1
  |
2 |     let x = 1
  |              ^ help: add `;` here
```

**先读最后一行 help,再往上看。** 大部分初学者问题都能照建议改掉。

---

## 6. 一个最小可运行的实验

在仓库里新建 `examples/hello.rs`(cargo 会把 `examples/*.rs` 当作示例程序编译):

```rust
//! 最小示例:验证工具链可用。

fn main() {
    let n: u32 = 3;
    println!("你好,mneme!n = {n}");
}
```

运行:

```bash
cargo run --example hello
```

输出:

```text
你好,mneme!n = 3
```

> `println!` 带感叹号说明它是一个**宏(macro)**,不是普通函数。`{n}` 是内联格式参数,
> 把变量 `n` 格式化后插进字符串。第 [02 章](02-values-and-ownership.md) 会再见到它。
>
> 同族的 `format!` 不打印,而是直接产出一个 `String`。格式参数还能带**宽度与补零**:
> `{month:02}` 按两位输出、不足补 0,`{milli:03}` 补到三位——L4 的 ISO 8601
> 格式化就用它拼出定宽时间戳(见 [`src/query/iso.rs:191-201`](../../src/query/iso.rs))。

---

## 7. 本章小结

- Rust 是编译型、静态强类型、无 GC 的内存安全语言;mneme 是一个库 crate。
- `rustup` 装工具链,`cargo` 负责构建/测试/文档/依赖。
- `Cargo.toml` 里:`edition` 是语法版本,`rust-version` 是 MSRV,`[dependencies]` 是运行依赖,
  `[dev-dependencies]` 只测试用;`"1"` 是 caret 语义版本。
- 日常四连:`cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`、`cargo doc`。
- mneme 用 `#![deny(missing_docs)]` 强制每个公开项都有文档。

## 动手练习

1. 运行 `cargo build`、`cargo test`、`cargo doc --no-deps`,观察分别发生了什么。
2. 故意删掉 `examples/hello.rs` 里的一处 `;`,看编译器怎么提示。
3. 打开 `Cargo.lock`,找到 `thiserror` 的精确版本号,和 `Cargo.toml` 里的 `"2"` 对比。

## 下一章

[02 值、类型与所有权](02-values-and-ownership.md):Rust 最核心、也最与众不同的部分。
