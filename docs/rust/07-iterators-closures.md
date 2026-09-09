# 07 迭代器与闭包

> **本章目标**:掌握 `for` 循环、迭代器链式调用(adapter)、闭包,以及 `sort_by` + `Ordering`。
> **前置**:[04 章](04-borrowing-strings-slices.md)(引用)、[06 章](06-generics-traits.md)(trait)。
> **对应源码**:[`src/core/simd.rs`](../../src/core/simd.rs)、[`src/core/heap.rs`](../../src/core/heap.rs)、
> [`src/core/varint.rs`](../../src/core/varint.rs)。

Rust 的迭代器是**惰性(lazy)**的:你写一串转换,只有到"消费"时(如 `sum`、`collect`、`for`)
才真正执行。它既表达力强,又能被编译器优化到和手写循环一样快。

---

## 1. `for` 循环与区间

```rust
for i in 0..5 {          // 0,1,2,3,4(左闭右开)
    println!("{i}");
}
for i in 0..=5 { ... }   // 0..=5 含 5(左闭右闭)
for i in (1..10).step_by(2) { ... }   // 1,3,5,7,9
```

`a..b` 和 `a..=b` 是 **Range**,本身就是迭代器。范围还能用于"是否包含":

```rust
if (Self::MIN..=Self::MAX).contains(&value) { ... }
```

见 [`src/core/options/dimension.rs:41`](../../src/core/options/dimension.rs)。

---

## 2. 迭代器的三种取得方式

| 方法 | 产生 | 说明 |
|---|---|---|
| `iter()` | `&T` | 只读借用,不消耗集合 |
| `iter_mut()` | `&mut T` | 可变借用 |
| `into_iter()` | `T` | 消耗集合,交出所有权 |

```rust
let v = vec![1, 2, 3];
for x in &v { }        // 等价于 v.iter(),x: &i32
for x in v.iter() { }
for x in v { }         // 等价于 v.into_iter(),x: i32,v 被消耗
```

> **edition 差异**:`for x in array` 自 Rust 1.53 起就按值迭代(`x: T`);而 `array.into_iter()`
> 这个**方法调用**在 2021 之前的 edition 里会解析成切片迭代、拿到 `&T`,2021+ 才按值。本项目用
> `edition = "2024"`,两者都按值。若看到旧教程说"数组迭代拿到引用",那说的是旧 edition 的方法调用。

---

## 3. 迭代器适配器(adapter)

适配器接收一个迭代器、返回一个新迭代器,可以链式组合。

```rust
a.iter()                       // &f32
 .zip(b.iter())                // (&f32, &f32)
 .map(|(x, y)| x * y)          // f32
 .sum::<f32>()                 // 消费:求和
```

这是 mneme 的标量点积实现,见 [`src/core/simd.rs:69-71`](../../src/core/simd.rs):

```rust
pub fn dot_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}
```

> `zip` 在**较短的迭代器耗尽时停止**,所以 `dot_scalar` 对长度不等的切片按较短者计算,不会 panic。
> `dot`(SIMD 版)也显式用 `n = a.len().min(b.len())` 保持同样语义;两者只对等长向量在 debug 下断言。

常用适配器:

| 适配器 | 作用 |
|---|---|
| `map(f)` | 把每个元素变成另一个值 |
| `filter(f)` | 只保留 `f` 返回 `true` 的元素 |
| `zip(other)` | 把两个迭代器配成对 |
| `enumerate()` | 加上下标,产出 `(usize, T)` |
| `take(n)` / `skip(n)` | 取前 n 个 / 跳过前 n 个 |
| `flat_map(f)` | 每个元素展开成多个 |
| `peekable()` | 可以偷看下一个元素 |

消费器(终结适配器):

| 消费器 | 作用 |
|---|---|
| `collect()` | 收集成 `Vec`/`HashMap` 等 |
| `sum()` / `product()` | 求和 / 求积 |
| `fold(init, f)` | 带累加器的归约 |
| `count()` | 计数 |
| `any(f)` / `all(f)` | 是否任一 / 全部满足 |
| `find(f)` / `position(f)` | 找第一个满足的 |
| `min()` / `max()` | 极值(要求 `Ord`;`f32` 不能直接用,见 [03 §4.1](03-structs-enums-impl.md)) |

### 3.1 `enumerate` 的例子

varint 解码要同时知道字节和它的位置:

```rust
for (index, &byte) in input.iter().enumerate() {
    ...
    if byte & CONTINUATION_BIT == 0 {
        return Ok((result, index + 1));
    }
}
```

见 [`src/core/varint.rs:96-108`](../../src/core/varint.rs)。`&byte` 是把 `&u8` 解构成 `u8`(因为 `u8` 是 `Copy`)。

### 3.2 `collect` 与类型标注

```rust
let v: Vec<i32> = (0..5).map(|x| x * x).collect();
```

`collect` 能收集成多种容器,所以常需要标注目标类型,或靠上下文推断。

---

## 4. 闭包(closure)

闭包是**可以捕获周围变量的匿名函数**:

```rust
let factor = 2.0;
let double = |x: f32| x * factor;   // 捕获了 factor
double(3.0);                         // 6.0
```

- `|参数| 表达式` 是闭包语法。
- 闭包能借用或移动它捕获的变量;用 `move` 强制把所有权移进闭包:

```rust
let s = String::from("hi");
let f = move || println!("{s}");     // s 被移动进闭包
```

### 4.1 闭包作为参数

`sort_by` 接收一个比较闭包:

```rust
self.heap.sort_by(|a, b| {
    if Self::is_better(metric, a.score, &a.payload, b.score, &b.payload) {
        std::cmp::Ordering::Less
    } else if Self::is_better(metric, b.score, &b.payload, a.score, &a.payload) {
        std::cmp::Ordering::Greater
    } else {
        std::cmp::Ordering::Equal
    }
});
```

见 [`src/core/heap.rs:168-176`](../../src/core/heap.rs)。

- 闭包参数 `a`、`b` 是 `&Entry<T>`(因为 `sort_by` 传引用)。
- 返回值是 `std::cmp::Ordering`,三选一:`Less`(a 在前)、`Greater`(b 在前)、`Equal`。
- 这里不能用简单的 `a.score.partial_cmp(&b.score)`,因为 mneme 的排序方向由 `Metric::better` 决定,
  且同分要按载荷升序。

> **`sort_by` 的比较闭包必须是全序**,否则可能 panic 或结果错乱。这也是不能直接写
> `a.score.partial_cmp(&b.score).unwrap()` 的原因:`f32` 的 `partial_cmp` 遇到 `NaN` 返回 `None`,
> `unwrap()` 会 panic。mneme 绕开浮点比较,改用 `Metric::better` + `Ord` 载荷保证全序:
> 对任意 `a`、`b`,`is_better(a, b)` 与 `is_better(b, a)` 至多一个为真,相等时再用
> `a_payload < b_payload` 兜底。见 [`src/core/heap.rs:180-195`](../../src/core/heap.rs)。

### 4.2 闭包捕获与借用规则

闭包默认按"最小权限"捕获:只读就借 `&`,要改就借 `&mut`,要所有权就 `move`。
这依然受 [04 章](04-borrowing-strings-slices.md)的借用规则约束。

---

## 5. `Option` 与数组:都是"可迭代"的(常见组合)

`Option<T>` 实现了 `IntoIterator`(注意:不是 `Iterator`),产出 0 或 1 个元素。因此可以:

```rust
let v: Vec<i32> = [Some(1), None, Some(3)].into_iter().flatten().collect();
// v == [1, 3]
```

数组则经 `IntoIterator` 进入 `for` 循环(即 §2 表中的 `into_iter` 一行,拿到的是元素值)。
mneme 的测试里也常见 `for (score, id) in [...]` 直接遍历数组,见
[`src/core/heap.rs:250-259`](../../src/core/heap.rs)。

---

## 6. 性能提示

- 迭代器链是**惰性且零开销**的:编译器通常会内联成与手写循环等价的机器码。
- 但过度嵌套会降低可读性;mneme 规范允许在算法清晰性优先时使用显式 `while` 循环
  (如 SIMD 内核里为了控制分块,见 [09 章](09-cfg-unsafe-simd.md))。

---

## 7. 你会遇到的编译器报错

| 报错关键词 | 原因 | 修法 |
|---|---|---|
| `value moved here, in previous iteration` | 在循环里移动了集合元素 | 用 `.iter()` 借用,或每次 clone |
| `cannot infer type` / `type annotations needed` | `collect` 目标类型不明 | 加 `let v: Vec<_>` 标注 |
| `closure may outlive the current function` | 闭包借用了局部变量但会逃逸 | 加 `move` |
| `cannot borrow ... as mutable` | 闭包与外部同时借用冲突 | 调整借用顺序 |
| `no method named map found for &Option<T>` | 在 `&Option<T>` 上调了 `map`(需要 `Option<T>`) | 先 `as_ref()`,或改为持有 `Option<T>` |

---

## 8. 本章小结

- `for` + Range 是基本循环;`iter`/`iter_mut`/`into_iter` 决定借用还是消耗。
- 迭代器适配器(`map`/`filter`/`zip`/`enumerate`)+ 消费器(`sum`/`collect`/`fold`)链式组合,惰性零开销。
- 闭包 `|x| ...` 能捕获环境;`move` 强制转移所有权。
- `sort_by` 配 `Ordering::{Less, Greater, Equal}` 做自定义排序;mneme 的 `TopK` 借此实现度量感知排序。

## 动手练习

1. 用一行迭代器求 `vec![1, 2, 3, 4, 5]` 中所有偶数的平方和。
2. 用 `enumerate` 打印一个 `Vec<&str>` 的 `下标:值`。
3. 用 `sort_by` 把 `vec![(2, "b"), (1, "a")]` 按第一个元素升序排序。

## 下一章

[08 模块、可见性与文档](08-modules-docs.md):代码怎么分文件、怎么暴露、怎么写文档。
