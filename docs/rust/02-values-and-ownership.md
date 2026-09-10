# 02 值、类型与所有权

> **本章目标**:掌握 Rust 的变量、基本类型,以及最重要的**所有权(ownership)**。
> **前置**:读过 [01 章](01-toolchain.md),会 `cargo build`。
> **对应源码**:[`src/core/types.rs`](../../src/core/types.rs)、[`src/core/varint.rs`](../../src/core/varint.rs)、
> [`src/core/metric.rs`](../../src/core/metric.rs)、[`src/memory/table.rs`](../../src/memory/table.rs)。

这是全书**最关键**的一章。所有权是 Rust 区别于其他语言的核心,也是初学者最容易卡住的地方。
读完本章你能理解 mneme 里为什么大量使用 `u32`/`u64`/`f32`、为什么 `newtype` 里直接包一个整数。

---

## 1. 变量:默认不可变

```rust
let x = 5;        // 绑定一个值;类型由编译器推断为 i32
let y: u32 = 5;   // 显式标注类型
// x = 6;         // 错误:默认不可变
let mut z = 5;    // 加 mut 才能改
z = 6;            // 正确
```

- `let` 是**绑定(binding)**,不是"赋值"。
- 默认**不可变(immutable)**。想改必须写 `mut`。这条规则能挡掉大量并发与逻辑错误。
- 类型通常可**推断(inference)**,写不出来时加 `: 类型`。

### 1.1 遮蔽(shadowing)

```rust
let x = 5;
let x = x + 1;    // 重新绑定,不是修改
let x = "现在是字符串";  // 甚至可以换类型
```

第二次 `let x` 会**遮蔽**第一次的 `x`。mneme 里常见于把 `String` 转成别的类型后复用名字。

### 1.2 常量

```rust
const COSINE_EPSILON: f32 = 1e-12;              // 编译期常量
const DEFAULT_TTL_MS: u64 = 30 * 60 * 1000;     // 允许简单运算
```

- 常量名用 **SCREAMING_SNAKE_CASE**,必须标注类型,值必须在编译期可算出。
- 与 `let` 的区别:常量可定义在任何作用域,没有固定地址(取引用会物化一个临时值),编译期被"内联"。
- mneme 把魔法数字提取成常量并注释来源,见 [`src/core/metric.rs:19`](../../src/core/metric.rs)、
  [`src/core/varint.rs:13-22`](../../src/core/varint.rs)。

---

## 2. 基本类型

### 2.1 整数

Rust 的整数按**有无符号**和**位宽**组合:

| 类型 | 位宽 | 取值范围(无符号) | mneme 里的用途 |
|---|---|---|---|
| `u8` | 8 | 0 … 255 | varint 的单字节 |
| `u16` | 16 | 0 … 65535 | `HnswParams::m`、文件格式版本 |
| `u32` | 32 | 0 … ~42 亿 | `SlotId`、`SegmentId`、`NsId`、维度 |
| `u64` | 64 | 0 … ~1.8e19 | `RowId`、`SeqNo`、字节数 |
| `usize` | 平台相关(64 位平台 = 64 位) | — | 集合长度、下标、切片索引 |

有符号对应 `i8`/`i16`/`i32`/`i64`/`isize`;`i64` 在 mneme 里用于 Unix 毫秒时间戳
(可为负,表示 1970 年之前)。

**为什么这么多整数类型?** Rust 不隐式转换。你不能把 `u32` 直接传给要 `u64` 的地方,必须显式
`as` 或 `u64::from`:

```rust
let small: u32 = 7;
let big: u64 = small as u64;        // 显式转换
let also: u64 = u64::from(small);   // 更安全的写法(见 [06 章] From)
```

mneme 规范要求**单位进名字**(`ttl_ms`、`size_bytes`),避免"这个 u64 到底是毫秒还是字节"。

#### 2.1.1 `as` 不是"安全转换",而是"截断"

`as` 在不同宽度的整数之间做的是**截断 / 符号扩展**,而且**不会报错**:

```rust
let big: u32 = 0x1234_5678;
let low: u8 = big as u8;          // 0x78:只保留低 8 位,高 24 位直接丢弃
let neg: i32 = -1;
let unsigned: u32 = neg as u32;   // 4294967295:补码按位重新解释
```

这正是 varint 编码里 `((value as u8) & PAYLOAD_MASK)` 能工作的原因:先 `as u8` 截出最低字节,
再用掩码取低 7 位。见 [`src/core/varint.rs:43`](../../src/core/varint.rs)。

反过来要特别小心:把 `u64 as u32`、`usize as u8` 用错会**静默丢数据**。所以:

- 能无损宽化时优先用 `u64::from(x)` / `u32::from(x)`(编译期保证不截断);
- 确实需要截断时才用 `as`,并在注释里写清"截断是有意的"。

> 时间戳在 mneme 里用 **`i64`**(可为负,表示 1970 年之前),不要因为"整数都用无符号"就写成 `u64`。

**整数溢出**:debug 构建下溢出会 panic;release 构建下会**回绕(wrapping)**。
所以涉及可能溢出的运算要小心,或使用 `checked_add` / `saturating_add` 等方法。
mneme 的 varint 解码显式检查溢出,见 [`src/core/varint.rs:99`](../../src/core/varint.rs)。

### 2.2 浮点

- `f32`(单精度,约 7 位有效数字)、`f64`(双精度)。
- mneme 的向量分量与分数统一用 `f32`(见 [`src/core/metric.rs:16`](../../src/core/metric.rs) 的
  `pub type Score = f32;`),因为向量计算量大、`f32` 带宽和速度更优。
- 浮点数比较不要用 `==`,要用误差阈值:

```rust
let s = 0.1_f32 + 0.2;
assert!((s - 0.3).abs() < 1e-6);   // 惯用写法
```

mneme 的测试里到处是 `(x - expected).abs() < 1e-6`,见 [`src/core/metric.rs:192`](../../src/core/metric.rs)。

### 2.3 布尔与字符

```rust
let is_enabled: bool = true;
let ch: char = '好';      // 单个 Unicode 标量值,占 4 字节
```

- 布尔变量按规范用 `is_` / `has_` / `can_` / `should_` 前缀。
- `char` 是**一个 Unicode 字符**,不是字节;`"好"` 是字符串字面量(`&str`,见 [04 章](04-borrowing-strings-slices.md))。

### 2.4 数组与元组

**数组(array)**:长度固定、元素同类型。

```rust
let a: [f32; 4] = [1.0, 2.0, 3.0, 4.0];
let b = [0.0_f32; 1536];      // 1536 个 0.0,长度写在类型里
let first = a[0];             // 下标访问
```

mneme 的 doctest 与测试里大量用定长数组,见 [`src/core/metric.rs:52-56`](../../src/core/metric.rs)。

**元组(tuple)**:长度固定、元素可不同类型。

```rust
let pair: (u64, usize) = (300, 2);   // 解码值 + 消耗字节数
let (value, consumed) = pair;        // 解构(destructuring)
let value2 = pair.0;                 // 或用 .0 / .1 访问
```

mneme 的 `decode_u64` 返回 `Result<(u64, usize)>`,见 [`src/core/varint.rs:93`](../../src/core/varint.rs)。

> 需要"可增长的同类型列表"用 `Vec<T>`,在 [07 章](07-iterators-closures.md) 讲。

---

## 3. 所有权(ownership):Rust 的核心

### 3.1 三条规则

1. 每个值都有一个**所有者(owner)**。
2. 同一时刻只能有这一个所有者,不能共享。
3. 所有者离开作用域时,值被**自动释放**(调用 `drop`)。

没有 GC,也没有手动 `free`——编译器在编译期算出该在哪里释放。

### 3.2 移动(move)

```rust
let s1 = String::from("hello");
let s2 = s1;          // s1 的所有权被"移动"给 s2
// println!("{s1}");  // 错误:s1 已被移动,不能再使用
println!("{s2}");     // 正确
```

对**堆分配**的类型(如 `String`、`Vec<T>`),赋值默认是**移动**而非拷贝。
移动后原变量失效,防止"两个变量指向同一块内存 → 双重释放"。

### 3.3 复制(copy)

对**栈上、大小固定、无资源语义**的类型,赋值是**复制**:

```rust
let a = 5;
let b = a;        // 复制;a 仍然可用
println!("{a} {b}");
```

哪些类型是 `Copy`?基本整数/浮点、`bool`、`char`;结构体/元组/数组**仅当所有字段都是
`Copy`、且显式派生**时才成立——实现 `Copy` 需要写 `#[derive(Copy, Clone)]`
(见 [03 章](03-structs-enums-impl.md))。

> `Copy` 必须与 `Clone` 一起派生(编译器会强制);`Eq`/`Ord` 也有类似的依赖链,而且
> **浮点类型不是 `Eq`/`Ord`**(因为 `NaN`)。这直接决定了 mneme 里哪些类型能排序、
> `TopK<T: Ord>` 的载荷能放什么——详见 [03 §4.1](03-structs-enums-impl.md)。

mneme 的标识类型就是 `Copy`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(u64);
```

见 [`src/core/types.rs:15`](../../src/core/types.rs)。因为 `u64` 是 `Copy`,所以 `RowId` 也是 `Copy`:
赋值/传参/放进 `Vec` 都按位复制,原变量依然可用,不存在"移动后失效"。

### 3.4 克隆(clone)

想显式复制一份堆数据,用 `.clone()`:

```rust
let s1 = String::from("hello");
let s2 = s1.clone();   // 深拷贝;s1 和 s2 各自独立
println!("{s1} {s2}");
```

- `.clone()` 可能分配内存,有成本;规范要求**借用优先,非必要不 `clone`**。
- 但**廉价克隆**是存在的:比如 `Arc<str>` 的 clone 只增加一个引用计数,不复制字符串内容
  (见 [04 章](04-borrowing-strings-slices.md))。

### 3.5 所有权与函数

把值传给函数,默认也是移动或复制:

```rust
fn takes_ownership(s: String) { /* s 在这里被拥有,函数结束即释放 */ }
fn makes_copy(n: u32) { /* u32 是 Copy,n 是复制 */ }

let s = String::from("hi");
takes_ownership(s);
// println!("{s}");  // 错误:s 已被移动
```

如果你**不想交出所有权**,就传**引用**(`&`),这是下一章的主题。mneme 的
`simd::dot(a: &[f32], b: &[f32])` 传的就是切片引用,不移动、不复制数据。

### 3.6 为什么 mneme 的 L0 几乎不用 `String`/`Vec` 作为字段

L0 原语层尽量用 `Copy` 的小类型(`u32`、`u64`、`f32`、`Metric`),让"所有权"几乎不构成负担:
这些类型复制即可,没有生命周期烦恼。等到需要共享字符串时,才引入 `Arc<str>`(见 [04 章](04-borrowing-strings-slices.md))。
这是**用类型选择降低复杂度**的典型工程取舍。

### 3.7 共享所有权:`Arc<T>` 与写时复制(COW)

L0 靠 `Copy` 小类型回避了所有权问题,但 L1(`src/memory/`)要在多个句柄、多个线程之间
共享同一份库状态:克隆一个 `Mneme` 不能把整个库复制一遍。这时用 **`Arc<T>`**
(Atomically Reference Counted,原子引用计数):

```rust
pub struct Mneme {
    pub(crate) table: Arc<Table>,
    pub(crate) config: Arc<Config>,
    pub(crate) control: CompactionControl,
}
```

见 [`src/memory/engine.rs`](../../src/memory/engine.rs)。`Mneme::clone()` 只是把每个 `Arc`
的计数 +1,所有克隆共享同一张表。用 `Arc::clone(&x)` 而不是 `x.clone()` 是刻意的:后者
容易让人误以为发生了深拷贝。

> `Arc` 与 `Rc` 的区别只在**引用计数是否原子**。原子操作略慢,但 `Arc` 能跨线程;
> mneme 面向并发,一律用 `Arc`。

**写时复制(clone-on-write)**:`Arc::make_mut(&mut x)` 是修改共享数据的入口——

- 若该 `Arc` 只有**一个所有者**,直接返回 `&mut`,原地改;
- 若**还有别处持有**(引用计数 > 1),先深拷贝一份、让当前所有者独享新副本,其他持有者
  继续看旧数据。

L1 写状态 `WriterState` 的全部容器字段都是 `Arc`,让"读者看到的永远是某个完整旧版本"成为可能:

```rust
pub(crate) fn hide_latest(&mut self, rowid: RowId) {
    if let Some(slot) = self.latest.get(&rowid).copied() {
        Arc::make_mut(&mut self.dead).set(slot.get() as usize);
    }
}
```

见 [`src/memory/table.rs`](../../src/memory/table.rs)。因此"给写状态拍快照"(`WriterState::clone`)
只是复制一批 `Arc` 句柄,非常廉价——这是写事务失败回滚与读者无锁扫描的共同前提
(用法见 [04 §5.1](04-borrowing-strings-slices.md) 与 [07 §4.3](07-iterators-closures.md))。

> 记录体里的向量也用 `Arc<[f32]>`(`SlotData::vector`):克隆一条记录只加计数,只有真正
> 替换向量时才分配新数组。

---

## 4. 类型系统如何"让非法状态不可表示"

mneme 大量使用 **newtype**:用一个只有一个字段的元组结构体包住底层整数。

```rust
pub struct RowId(u64);
pub struct SlotId(u32);
```

好处:虽然底层都是整数,但 `RowId` 和 `SlotId` 是**不同的类型**,
把 `SlotId` 传给需要 `RowId` 的地方会**编译错误**,不会出现"把行号当槽位用"的运行时 bug。
详见 [`src/core/types.rs`](../../src/core/types.rs) 的模块文档,以及设计文档 [02 §1](../design/02-l0-core.md)。

> 这是 Rust 表达"领域约束"的常用手法,第 [03 章](03-structs-enums-impl.md) 会完整展开。

---

## 5. 你会遇到的编译器报错

| 报错关键词 | 原因 | 修法 |
|---|---|---|
| `cannot assign twice to immutable variable` | 想改没加 `mut` 的变量 | 加 `mut` |
| `borrow of moved value` | 值被移动后还想用 | 传引用、或先 `.clone()` |
| `use of possibly-uninitialized variable` | 变量没初始化就用 | 先赋值 |
| `mismatched types expected u64, found u32` | 整数类型不匹配 | 显式 `as` 或 `u64::from(...)` |
| `cannot move out of ... which is behind a shared reference` | 想从 `&T` 里拿走所有权 | 用 `.clone()`,或改成借用 |

---

## 6. 本章小结

- `let` 默认不可变,要改加 `mut`;常量用 `const` + 全大写下划线。
- 整数按位宽/符号分很多种,Rust **不做隐式转换**;浮点比较用误差阈值。
- 数组定长、元组可异构;需要可增长列表用 `Vec`。
- **所有权**:每个值一个所有者,离开作用域自动释放;赋值对堆类型是**移动**,对 `Copy` 类型是复制;
  想复制堆数据用 `.clone()`。
- **共享所有权**用 `Arc<T>`(`Arc::clone` 只加计数);要改共享数据用 `Arc::make_mut` 写时复制。
- newtype 用类型区分语义,把错误挡在编译期。

## 动手练习

1. 在 `examples/hello.rs` 里声明 `let x: u32 = 5;`,试着传给一个需要 `u64` 的函数,读报错并修正。
2. 写 `let s1 = String::from("a"); let s2 = s1;`,再打印 `s1`,观察移动报错;改成 `s1.clone()` 后通过。
3. 给 `examples/hello.rs` 加一个 `const MAX_ROWS: u32 = 1_000_000;` 并用 `println!` 打印。

## 下一章

[03 结构体、枚举与 impl](03-structs-enums-impl.md):用类型描述数据,用 `impl` 给类型挂行为。
