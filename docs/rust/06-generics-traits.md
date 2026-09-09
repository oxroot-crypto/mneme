# 06 泛型与 trait

> **本章目标**:理解泛型参数、trait 与 trait bound,读懂 `TopK<T: Ord>` 和 `Clock` 这类签名,
> 并会自己实现标准 trait(`From`、`Display`、`Default`)。
> **前置**:[03 章](03-structs-enums-impl.md)(`impl`)、[05 章](05-errors.md)。
> **对应源码**:[`src/core/heap.rs`](../../src/core/heap.rs)、[`src/core/options/clock.rs`](../../src/core/options/clock.rs)、
> [`src/core/types.rs`](../../src/core/types.rs)、[`src/core/metric.rs`](../../src/core/metric.rs)。

**泛型(generics)** 让一套代码适配多种类型;**trait** 定义"一个类型能做什么";
**trait bound** 给泛型参数加上"必须能做什么"的限制。三者合起来,是 Rust 的抽象与复用机制。

---

## 1. 泛型参数

```rust
pub struct TopK<T: Ord> {
    k: usize,
    metric: Metric,
    heap: Vec<Entry<T>>,
}
```

见 [`src/core/heap.rs:27-31`](../../src/core/heap.rs)。

- `T` 是**类型参数**,占位符;用 `TopK<u32>` 时 `T = u32`。
- 可以实例化成 `TopK<u32>`、`TopK<RowId>`……同一份代码复用。
- 泛型是**零成本抽象**:编译时为每个具体类型生成一份代码,运行时没有额外开销。

泛型函数同理:

```rust
fn identity<T>(value: T) -> T { value }
```

> 命名:mneme 规范偏好语义化名(`TItem`、`TError`),单字母 `T` 也接受。

---

## 2. trait:定义"能做什么"

**trait** 类似其他语言的接口(interface):声明一组方法,类型通过 `impl Trait for Type` 实现。

```rust
pub trait Clock: Send + Sync {
    fn now_unix_ms(&self) -> i64;
}
```

见 [`src/core/options/clock.rs:9-16`](../../src/core/options/clock.rs)。

- `Clock` 是 trait 名,里面声明了方法 `now_unix_ms`(只有签名,没有实现)。
- `: Send + Sync` 是 **supertrait(父 trait)约束**:任何 `Clock` 的实现者还必须满足 `Send + Sync`。
- 实现:

```rust
impl Clock for SystemClock {
    fn now_unix_ms(&self) -> i64 { ... }
}
```

见 [`src/core/options/clock.rs:22-29`](../../src/core/options/clock.rs)。

### 2.1 trait 也能提供默认实现

```rust
trait Greet {
    fn name(&self) -> String;
    fn hello(&self) -> String {          // 默认实现
        format!("你好,{}", self.name())
    }
}
```

实现者只需提供 `name`,就能免费得到 `hello`。

### 2.2 泛型 + trait bound

```rust
impl<T: Ord> TopK<T> {
    pub fn push(&mut self, score: Score, payload: T) { ... }
}
```

`T: Ord` 读作"T 必须实现 `Ord` trait"。为什么需要?因为 `TopK` 在分数相同时要按载荷
`a_payload < b_payload` 排序,`<` 来自 `Ord`。没有这个约束,编译器不知道 `T` 能否比较大小。

见 [`src/core/heap.rs:33`](../../src/core/heap.rs) 与 [`src/core/heap.rs:193`](../../src/core/heap.rs)。

等价写法(更复杂时用 `where`):

```rust
impl<T> TopK<T>
where
    T: Ord,
{ ... }
```

### 2.3 `impl Trait` 参数:简化写法

```rust
pub fn new(value: impl Into<Arc<str>>) -> Self {
    Self(value.into())
}
```

见 [`src/core/types.rs:235`](../../src/core/types.rs)。`impl Into<Arc<str>>` 是"接受任何实现了
`Into<Arc<str>>` 的类型"的简写,等价于:

```rust
pub fn new<T: Into<Arc<str>>>(value: T) -> Self { ... }
```

于是 `Key::new("x")` 和 `Key::new(String::from("x"))` 都能用(见 [04 章 §3.1](04-borrowing-strings-slices.md))。

> **`impl Trait` 参数 vs 泛型参数怎么选?** 两者几乎等价,但有区别:
> - `impl Trait` 更短、调用者不用写类型;但不能在调用点用 turbofish 指定类型(`f::<T>(...)`),
>   也不能要求多个参数是"同一类型"。
> - 泛型参数可以写 `fn f<T: Trait>(a: T, b: T)` 强制 `a`、`b` 同类型;`impl Trait` 写不出来。
> - 多个复杂约束用 `where` 更易读。

---

## 3. 标准库常用 trait

| trait | 能力 | 对应语法/方法 | mneme 里 |
|---|---|---|---|
| `Debug` | 调试打印 | `{:?}` | 几乎所有类型都 `derive` |
| `Display` | 用户友好打印 | `{}`、`.to_string()` | `RowId`、`Key` 手写 |
| `Clone` | 显式复制 | `.clone()` | 到处 |
| `Copy` | 赋值即复制 | — | `RowId` 等小类型 |
| `PartialEq`/`Eq` | 相等比较 | `==` | 标识类型 |
| `PartialOrd`/`Ord` | 排序 | `<`、`.sort()` | `TopK<T: Ord>` |
| `Hash` | 可哈希 | 放进 `HashMap` | 标识类型 |
| `Default` | 默认值 | `T::default()` | 配置类型 |
| `From`/`Into` | 类型转换 | `T::from(x)` / `x.into()` | `Key`、`MnemeError` |
| `Send`/`Sync` | 可跨线程 | — | `Clock` 约束 |

### 3.1 `From` 与 `Into`

标准库规定:实现了 `From<A> for B`,就自动获得 `Into<B> for A`。

```rust
impl From<u64> for RowId {
    fn from(value: u64) -> Self { Self(value) }
}
```

见 [`src/core/types.rs:42-46`](../../src/core/types.rs)。于是:

```rust
let a = RowId::from(42_u64);   // From
let b: RowId = 42_u64.into();  // Into(自动获得)
```

`thiserror` 的 `#[from]` 就是自动生成 `From<底层错误> for MnemeError`(见 [05 章 §3.2](05-errors.md))。

> **孤儿规则(orphan rule)**:你只能为自己的类型实现外部 trait,或为外部类型实现自己的 trait,
> 不能给外部类型实现外部 trait(比如给 `Vec<u8>` 实现 `Display`)。`Key` 是本地类型,所以
> `impl From<&str> for Key` 合法。

### 3.2 `Display`

```rust
impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
```

见 [`src/core/types.rs:48-52`](../../src/core/types.rs)。`write!` 与 `println!` 用法一致,
只是写到 `f` 而不是标准输出。

### 3.3 `Send` / `Sync`:线程安全标记

- `Send`:值可以**移动到**另一个线程。
- `Sync`:值的**共享引用** `&T` 可以跨线程。
- 它们是**自动 trait(auto trait)**,没有方法,由编译器按字段自动推导(所有字段都 `Send`/`Sync`,整个类型才自动满足)。
- `Clock: Send + Sync` 保证时钟对象能在多线程环境安全使用。

---

## 4. 泛型 vs trait 对象

- **泛型**(静态分发):`TopK<T>`——编译期为每个 `T` 生成代码,无运行时开销,但会增加二进制体积。
- **trait 对象**(动态分发):`Box<dyn Clock>`——运行期通过虚表调用,可存放不同类型,但有间接开销。

mneme 的 `Clock` 是 trait;需要"运行时可替换的时钟"时,可以用 `Box<dyn Clock>` 或泛型参数。
本教程不必深入,记住:**优先泛型,trait 对象用于确需异构集合时**。

### 4.1 单态化:泛型"零成本"是怎么来的,代价又是什么

编译器在编译期为**每一个实际用到的类型参数**复制一份泛型代码,这叫**单态化(monomorphization)**:

```rust
fn identity<T>(value: T) -> T { value }

identity(1_u32);       // 生成 identity::<u32>
identity(1_u64);       // 再生成 identity::<u64>
```

- 好处:没有虚表、没有间接跳转,和手写两个 `u32`/`u64` 函数一样快——这就是"零成本抽象"。
- 代价:**代码膨胀**。`TopK<u32>` 和 `TopK<RowId>` 是两份机器码;泛型嵌套或实例很多时二进制会变大。
- 动态分发(`dyn`)反过来:一份代码、运行时查虚表,体积小但有间接开销,且**无法内联**。

**取舍**:热路径、类型集合已知时用泛型;需要"同一个容器装不同类型"或想控制代码体积时用 trait 对象。
mneme 的 `TopK<T: Ord>` 是热路径泛型;`Clock` 只在边界注入,两种都可。

---

## 5. 关联类型(了解即可)

trait 可以带**关联类型**:

```rust
trait Iterator {
    type Item;                        // 关联类型
    fn next(&mut self) -> Option<Self::Item>;
}
```

[07 章](07-iterators-closures.md)会大量用到 `Iterator`,届时 `Item` 会自然出现。

---

## 6. 你会遇到的编译器报错

| 报错关键词 | 原因 | 修法 |
|---|---|---|
| `the trait bound `T: Ord` is not satisfied` | 泛型缺少约束 | 在 `<T: Ord>` 或 `where` 里加 |
| `the method ... exists for ... but its trait bounds were not satisfied` | 缺少某个 trait 实现 | `derive` 或手写 `impl` |
| `no method named X found for type T` | `T` 没有该 trait 的方法 | 加对应 trait bound |
| `the trait `Copy` cannot be implemented for this type` | 字段含非 `Copy` 类型 | 去掉 `Copy`,改用 `Clone` |
| `conflicting implementations` | 同一 trait 对同一类型实现两次 | 删除重复实现 |

---

## 7. 本章小结

- 泛型用 `<T>` 让代码复用,编译期单态化、零运行时开销。
- trait 定义"能做什么",`impl Trait for Type` 实现;trait 可带默认实现和 supertrait 约束。
- trait bound(`T: Ord` 或 `where T: Ord`)给泛型加能力要求;`impl Trait` 是简写。
- `From`/`Into` 互推、`Display` 手写、`Send`/`Sync` 是线程安全标记。
- mneme 的 `TopK<T: Ord>` 依赖 `Ord` 做同分排序;`Clock: Send + Sync` 保证跨线程安全。

## 动手练习

1. 写一个泛型函数 `fn largest<T: PartialOrd + Copy>(list: &[T]) -> T`,返回最大元素。
2. 给 [03 章练习](03-structs-enums-impl.md)的 `Point` 实现 `Default`(全 0)和 `From<(f32, f32)>`。
3. 把 `Point` 作为 `TopK<Point>` 的载荷——需要给 `Point` 派生什么 trait?为什么?

## 下一章

[07 迭代器与闭包](07-iterators-closures.md):用链式调用替代手写循环。
