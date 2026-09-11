# 05 错误处理

> **本章目标**:掌握 `Option`、`Result`、`?` 运算符、`match`/`if let`/`matches!`,
> 以及 mneme 用 `thiserror` 定义统一错误枚举的方式。
> **前置**:[03 章](03-structs-enums-impl.md)(枚举与 `impl`)。
> **对应源码**:[`src/core/error.rs`](../../src/core/error.rs)、[`src/core/varint.rs`](../../src/core/varint.rs)、
> [`src/core/options/dimension.rs`](../../src/core/options/dimension.rs)、[`src/core/meta.rs`](../../src/core/meta.rs)、
> [`src/index/hidx.rs`](../../src/index/hidx.rs)、[`src/index/graph.rs`](../../src/index/graph.rs)、
> [`src/query/json.rs`](../../src/query/json.rs)、[`src/query/iso.rs`](../../src/query/iso.rs)、
> [`src/query/zmap.rs`](../../src/query/zmap.rs)、[`src/query/plan.rs`](../../src/query/plan.rs)。

Rust 没有异常(exception)和 `try/catch`。它把"可能失败"编码进**类型**:

- **可能为空** → `Option<T>`;
- **可能失败** → `Result<T, E>`。

编译器强制你处理这两种情况,不能假装它们不存在。这正是 mneme "拒绝静默失败"契约的语言基础。

---

## 1. `Option<T>`:有或没有

`Option<T>` 是一个枚举,定义在标准库里:

```rust
enum Option<T> {
    Some(T),   // 有值
    None,      // 没有
}
```

### 1.1 构造与匹配

```rust
let x: Option<u32> = Some(7);
let y: Option<u32> = None;

match x {
    Some(v) => println!("有值:{v}"),
    None => println!("没有"),
}
```

### 1.2 常用方法

```rust
let n = Some(3);
n.unwrap_or(0);        // Some(3) -> 3,None -> 0(安全)
n.map(|v| v + 1);      // Some(4)
n.and_then(|v| if v > 0 { Some(v) } else { None });  // 链式
n.is_some();           // true
```

> 上面能连续调用 `n.*` 是因为 `Option<i32>` 是 `Copy`(内部 `i32` 可复制)。如果换成
> `Option<String>`,`n.unwrap_or(...)` 会把 `n` **移动**掉,后面再用就编译不过——要么先 `.clone()`,
> 要么改用 `n.as_ref().unwrap_or(...)` 这类借用式 API。
>
> 另外 `map`/`and_then` 返回的 `Option` 标了 `#[must_use]`:直接丢弃会有警告,提醒你"这个结果还没处理"。

L3 还大量使用几个"取值 + 兜底"的组合子:

```rust
let filter: Option<&BitSet> = None;
// is_none_or:None -> true;Some(v) -> f(v)
// 正好表达"没有过滤位图 = 不过滤,放行"
filter.is_none_or(|bits| bits.get(slot));
// 等价写法:filter.map_or(true, |bits| bits.get(slot))

let level: Option<u8> = None;
level.map_or(0, |l| l as usize);      // None 时给默认 0
```

见 [`src/index/filtered.rs:100-104`](../../src/index/filtered.rs) 与
[`src/index/hidx.rs:257-259`](../../src/index/hidx.rs)。`is_none_or` 是 Rust 1.82 稳定的
新方法,名字直译就是"是 `None` 或者满足条件";在"默认放行、有值才检查"的语义下比
`map_or(true, ...)` 更不容易读反。

把 `Option` 转成 `Result` 用 `ok_or` / `ok_or_else`——后者**只在失败时才构造错误**:

```rust
let expected_node_table = header
    .count
    .checked_mul(NODE_TABLE_ENTRY)
    .ok_or_else(|| corrupt("node_table_len 溢出"))?;
```

见 [`src/index/hidx.rs:243-246`](../../src/index/hidx.rs)。`checked_mul` 返回 `Option<usize>`
(见 [02 §2.1.2](02-values-and-ownership.md)),`ok_or_else` 把 `None` 变成
`Err(MnemeError::Corrupted { .. })`,`?` 再立即返回——这是"受检运算 + 结构化错误"的标准配合。

> 惰性的意义:`ok_or_else(|| ...)` 的闭包只在 `None` 时执行;`ok_or(...)` 会**先**把错误值
> 构造出来,成功路径也白付一次代价。错误里含 `format!` 时一律用 `ok_or_else`。

测试里常把 `Result` 的**错误值**取出来检查,惯用 `.err().expect(...)`:`.err()` 把
`Result<T, E>` 转成 `Option<E>`(`Ok` 变 `None`),于是可以像普通 `Option` 一样解包:

```rust
let error = HnswIndex::load(...)
    .err()
    .expect("节点数不一致必须拒绝载入");
assert!(matches!(error, MnemeError::Corrupted { .. }));
```

见 [`src/index/hnsw.rs:677-683`](../../src/index/hnsw.rs)。

mneme 的 `meta::get_path` 用 `?` 在 `Option` 上做链式短路:

```rust
for segment in path.split('.') {
    current = current.as_object()?.get(segment)?;
}
```

见 [`src/core/meta.rs:44-46`](../../src/core/meta.rs)。任一环返回 `None`,整个函数立即返回 `None`。

L4 又用到几个"判断 / 过滤 / 借用"的组合子:

```rust
// bool → Option:条件成立给 Some(v),否则 None(注意 v 会立即求值)
rest.is_empty().then_some(0);

// is_some_and:Some 且谓词为真(与 is_none_or 正好互补)
ctx.view.zones
    .block_stat(field, block)
    .is_some_and(|stat| stat.has_null);

// filter:Some 且满足谓词才保留,否则变 None
value.as_f64().filter(|value| value.is_finite());

// as_deref:Option<String> → Option<&str>,借用式读取、不移动
self.text.as_deref();

// cloned:Option<&T> → Option<T>,复制出拥有值(HashMap::get 返回引用)
via_map.get(&rowid).cloned();

// map_or_else:两个分支都是惰性闭包
filter.map_or_else(
    || zmap::full_mask(zmap::block_count(view)),
    |expr| zmap::block_mask(expr, view),
);
```

见 [`src/query/iso.rs:94`](../../src/query/iso.rs)、
[`src/query/zmap.rs:148-164`](../../src/query/zmap.rs)、
[`src/query/json.rs:101-104`](../../src/query/json.rs)、
[`src/query/exec.rs:368-369`](../../src/query/exec.rs) 与
[`src/query/plan.rs:44-47`](../../src/query/plan.rs)。逐条:

- `then_some`:等价于 `if cond { Some(v) } else { None }`,但 `v` **立即求值**;
  要惰性(只在 `true` 时才算)就用 `bool::then(|| ...)`。
- `is_some_and`:表达"有值且满足条件"。上文 `zmap` 用它判断"该块存在此字段的摘要且摘要含 null"。
- `filter`:保留满足谓词的 `Some`,其余变 `None`——JSON 解码用它把非有限 `f64` 直接滤掉。
- `as_deref`:`Option<String>`(或 `&Option<String>`)→ `Option<&str>`,不移动内部值;
  `Option<Vec<T>>` → `Option<&[T]>` 同理。
- `cloned()`:`Option<&T>` → `Option<T>`(要求 `T: Clone`);`HashMap::get` 返回引用,
  L4 取来源边时用 `via_map.get(&rowid).cloned()` 复制出拥有值(见
  [`src/query/exec.rs:357`](../../src/query/exec.rs))。
- `map_or_else(none_fn, some_fn)`:两个分支都惰性;L4 计划器用它"无过滤→全 1 位图,
  有过滤→下推求值"。默认值构造昂贵时,`map_or`(默认值立即求值)不合适。

### 1.3 `unwrap()` / `expect()`:危险动作

```rust
let v = Some(3).unwrap();          // 是 Some 就返回内部值;是 None 就 panic!
let v = Some(3).expect("必须有值"); // 同上,panic 时带自定义信息
```

- `unwrap` / `expect` 在 `None` 时**直接 panic**,程序崩溃。
- mneme 规范:**生产代码禁止 `unwrap`/`expect`/`panic!`**,除非能证明不会失败并注释原因。
- 测试和文档示例里可以放宽使用(测试里失败就是失败,信息更直接)。

---

## 2. `Result<T, E>`:成功或错误

```rust
enum Result<T, E> {
    Ok(T),    // 成功,带值
    Err(E),   // 失败,带错误
}
```

mneme 给 `Result` 起了别名,固定错误类型:

```rust
pub type Result<T> = std::result::Result<T, MnemeError>;
```

见 [`src/core/error.rs:133`](../../src/core/error.rs)。于是全库函数签名统一写成 `-> Result<T>`,
不需要每次都写 `, MnemeError`。

### 2.1 `match` 处理

```rust
match Dimension::new(1536) {
    Ok(d) => println!("维度 = {}", d.get()),
    Err(e) => eprintln!("失败:{e}"),
}
```

### 2.2 `?` 运算符:错误的自动传播

`?` 放在返回 `Result` 的函数里,作用:

- 是 `Ok(v)` → 取出 `v` 继续;
- 是 `Err(e)` → **立即返回** `Err(e)`(经过 `From` 转换,见 §3.2)。

```rust
pub fn new(value: u32) -> Result<Self> {
    if (Self::MIN..=Self::MAX).contains(&value) {
        Ok(Self(value))
    } else {
        Err(MnemeError::LimitExceeded {
            field: "dimension",
            limit: Self::MAX as usize,
            got: value as usize,
        })
    }
}
```

见 [`src/core/options/dimension.rs:40-46`](../../src/core/options/dimension.rs)。

带 `?` 的例子(varint 解码逐字节,遇到畸形就返回):

```rust
let low = u64::from(byte & PAYLOAD_MASK);
if shift == 63 && low > U64_LAST_GROUP_MAX {
    return Err(corrupted("varint 数值溢出 u64"));
}
```

见 [`src/core/varint.rs:96-104`](../../src/core/varint.rs)。

### 2.3 `?` 也能用在 `Option` 上

在返回 `Option` 的函数里,`?` 遇到 `None` 就返回 `None`(见 §1.2 的 `get_path`)。

### 2.4 把 `Result` 的迭代器收成 `Result<Vec<_>>`

L4 的 JSON 解码要批量转换数组元素,失败就整体失败:

```rust
let vals: Result<Vec<Val>> = items.iter().map(val_from_meta).collect();
```

见 [`src/query/json.rs:166-167`](../../src/query/json.rs)。当 `collect` 的目标类型是
`Result<Vec<T>, E>` 时,迭代器元素必须是 `Result<T, E>`:`Ok` 逐个收集,一旦遇到
`Err` 就**立即返回该 `Err`**、丢弃已收集的部分。这是"批量 `?`"的惯用写法,
比手写循环 + `?` 短得多。反过来也行:`Vec<Option<T>>` 可收成 `Option<Vec<T>>`。

> `collect` 能变出什么,完全由**标注的目标类型**决定(见
> [07 §3.2](07-iterators-closures.md)):同一串迭代器可以收成 `Vec`、`HashMap`、
> `Result`、`Option`……编译器靠类型标注来选。

---

## 3. 定义自己的错误:`thiserror`

手写 `Error` trait 的样板很烦。mneme 用 `thiserror` 的派生宏:

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MnemeError {
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("向量维度不匹配:期望 {expected},实际 {got}")]
    DimensionMismatch {
        expected: u32,
        got: usize,
    },
    ...
}
```

见 [`src/core/error.rs:16-34`](../../src/core/error.rs)。

### 3.1 `#[error("...")]`:错误消息模板

- `{0}` 引用第 0 个元组字段;`{expected}` 引用结构体字段。
- 可以内联变量名(如 `{got}`),也可以用表达式。
- 派生后自动实现 `Display` 和 `std::error::Error`。

### 3.2 `#[from]`:自动转换

`Io(#[from] std::io::Error)` 会生成 `From<std::io::Error> for MnemeError`。
于是 `?` 遇到 `std::io::Error` 时能**自动转换**成 `MnemeError::Io(...)`,不用手写 `map_err`。

#### 3.2.1 `?` 的展开:它其实是一次 `match` + `From::from`

`?` 不是魔法,`expr?` 大致展开为:

```rust
match expr {
    Ok(value) => value,
    Err(error) => return Err(From::from(error)),   // 关键:自动 From 转换
}
```

所以 `?` 能把底层错误(如 `std::io::Error`)自动转成 `MnemeError`,靠的正是 `thiserror` 的
`#[from]` 生成的 `From<std::io::Error> for MnemeError`。如果某个错误类型没有对应的 `From`,
`?` 就会编译失败(`the trait From<...> is not implemented`),这时要么补 `#[from]`,要么手写
`.map_err(...)`。

在返回 `Option` 的函数里,`?` 展开成"遇到 `None` 就 `return None`",不做 `From` 转换。

### 3.3 错误上下文

mneme 规范要求错误携带**定位所需的信息**:

```rust
#[error("数据损坏(段 {segment:?}): {reason}")]
Corrupted {
    segment: Option<SegmentId>,   // None 表示文件级损坏
    reason: String,
},
```

见 [`src/core/error.rs:19-26`](../../src/core/error.rs)。报错时你能看到"哪一段、为什么",
而不是一句笼统的 "corrupted"。

### 3.4 `#[source]` 与根因链

当错误由底层错误引起时,用 `#[source]` 或 `#[from]` 保留**根因**,方便逐层追溯。
`#[from]` 隐含了 `#[source]`。

---

## 4. `match`、`if let`、`matches!` 与 `let` 家族

### 4.1 `match` 穷尽所有情况

```rust
match result {
    Ok(v) => v,
    Err(MnemeError::Io(e)) => panic!("IO: {e}"),
    Err(other) => return Err(other),
}
```

### 4.2 `if let`:只关心一个分支

```rust
if let Some(v) = maybe {
    println!("{v}");
}
```

等价于只写 `match` 的 `Some` 分支,忽略其余。

`if let` 的模式还能直接解构引用。例如 L3 这段:

```rust
if let Some(&(_, best)) = candidates.first() {
    entry = best;
}
```

见 [`src/index/hnsw.rs:355-357`](../../src/index/hnsw.rs)。`candidates.first()` 返回
`Option<&(Score, u32)>`(切片首元素的引用);模式最前面的 `&` 把引用"拆开",`(_, best)`
再解出元组第二个字段,`_` 忽略分数。于是 `best` 是复制出来的 `u32`,不是引用。
规则:模式要对**值的类型**匹配,加 `&` 可以"透过引用看进去"——和 [07 §3.1](07-iterators-closures.md)
的 `for (index, &byte) in ...` 是同一个原理。

### 4.3 `matches!`:返回布尔值

```rust
pub const fn needs_norm(&self) -> bool {
    !matches!(self, Metric::Dot)
}
```

见 [`src/core/metric.rs:108-110`](../../src/core/metric.rs)。`matches!` 判断值是否匹配某个模式,
比手写 `match { ... => true, _ => false }` 简洁。

### 4.4 测试里的 `matches!`

```rust
assert!(matches!(err, MnemeError::Corrupted { .. }));
```

`{ .. }` 表示"结构体变体,字段随便"。见 [`src/core/varint.rs:219-222`](../../src/core/varint.rs)。

### 4.5 `while let`:循环地匹配一个分支

`if let` 只判断一次;`while let` 反复判断,直到模式不再匹配:

```rust
while let Some(current) = frontier.pop() {   // 堆非空就弹出一个继续搜
    ...
}
```

见 [`src/index/hnsw.rs:241`](../../src/index/hnsw.rs)。`pop()` 返回 `Option<Cand>`:
`Some` 进入循环体,`None`(堆空)结束循环。它等价于:

```rust
loop {
    match frontier.pop() {
        Some(current) => { ... }
        None => break,
    }
}
```

> 什么时候用 `while let`:消费一个"每次取出一个、可能取空"的来源——堆的 `pop`、迭代器的
> `next`、通道的 `recv`。前提是**循环必然终止**:这里的堆每 `pop` 一次少一个元素,
> 终会走到 `None`。

### 4.6 `let...else`:匹配失败就提前退出

当"匹配成功继续、失败立刻返回"时,`let...else` 比 `if let` 少一层缩进,也不会把后续代码
关进花括号:

```rust
let Ok(decoded) = decode(&bytes) else {
    return Ok(());      // 解码失败:本用例视为不适用,直接结束
};
// 从这里起,decoded 一定可用
```

见 [`src/index/hidx.rs:780-782`](../../src/index/hidx.rs)。规则:

- `else` 块**必须发散(diverge)**:里面必须以 `return`/`break`/`continue`/`panic!` 结束,
  编译器才能保证"走到下面时模式一定匹配成功";
- 成功绑定(`decoded`)在整条语句之后都可用,不像 `if let` 的绑定被限制在块内;
- 常见于"先解构出结果,失败就早退"的卫语句写法,与 [CONTRIBUTING.md](../../CONTRIBUTING.md)
  要求的"提前返回、拒绝深嵌套"相配。

### 4.7 `let` 链:多个条件连续匹配

`edition = "2024"` 起,`if let` / `while let` 可以和普通条件、其它 `let` 用 `&&` 串起来:

```rust
if let Some(links) = self.nodes.get_mut(node as usize)
    && let Some(slot) = links.neighbors.get_mut(level)
{
    *slot = neighbors;
}
```

见 [`src/index/graph.rs:70-76`](../../src/index/graph.rs)。要点:

- 只有前面的条件为真,后面的 `let` 才执行;任一失败整个 `if` 为假——语义就是短路,
  前面的 `Option` 是 `None` 时后面的表达式根本不求值;
- 每层解构出的绑定,在后续条件和块里都可用(`links` 在第二个 `let` 里就能用);
- 它取代嵌套的 `if let` 金字塔,可读性更好;**只在 `edition = "2024"` 可用**
  (mneme 正是 2024,见 [01 §4.3](01-toolchain.md))。

---

## 5. panic 还是 Result?mneme 的取舍

| 场景 | 处理方式 |
|---|---|
| 可预期的失败(坏输入、IO、维度不符) | 返回 `Result`,由调用方决定 |
| 编程错误 / 违反不变量(如索引越界) | `panic!`,表示 bug |
| 库代码 | **禁止 panic**,返回结构化错误 |
| 测试代码 | 可以 `unwrap`/`expect`/`panic!` |

mneme 的 L0 契约明确:**公开 API 不 panic**(FC-CORE-INV-002,见
[`tests/core_contracts.rs:151`](../../tests/core_contracts.rs))。甚至浮点的边界也处理成确定值:

```rust
// 余弦零向量返回 0,绝不返回 NaN
if denominator < COSINE_EPSILON { 0.0 } else { simd::dot(a, b) / denominator }
```

见 [`src/core/metric.rs:137-144`](../../src/core/metric.rs)。

> **"拒绝静默失败"**:不能吞掉错误(如 `let _ = ...`),也不能把坏输入当正常数据处理。
> 要么返回带上下文的错误,要么(仅在不可恢复时)panic。

---

## 6. 你会遇到的编译器报错

| 报错关键词 | 原因 | 修法 |
|---|---|---|
| `the `?` operator can only be used ... returning Result` | 在非 `Result` 函数里用 `?` | 改返回类型为 `Result`,或用 `match` |
| `mismatched types expected Result, found T` | 忘记 `Ok(...)` 包装 | 用 `Ok(value)` |
| `unused `Result` that must be used` | 拿到 `Result` 没处理 | 用 `?`、`match`、或显式 `.ok()` 并注释 |
| `no variant named X` | 枚举变体名写错 | 对照定义 |
| `non-exhaustive patterns` | `match` 漏分支 | 补全,或加 `_`(库外部类型) |
| `refutable pattern in local binding` | 用 `let` 匹配了可能失败的模式 | 补 `else`(`let...else`),或改用 `if let`/`match` |

---

## 7. 本章小结

- `Option<T>` 表示"有/无",`Result<T, E>` 表示"成功/失败",都由编译器强制处理。
- `?` 在 `Result` 或 `Option` 上短路传播;`unwrap`/`expect` 会 panic,生产代码禁用。
- `map_or`/`is_none_or`/`ok_or_else` 等组合子把"取值 + 兜底 / 转错误"写成表达式,
  `ok_or_else` 惰性构造错误,优先于 `ok_or`;L4 还常用 `then_some`、`is_some_and`、
  `filter`、`as_deref`、`cloned`、`map_or_else`(两分支惰性)。
- `collect::<Result<Vec<_>>>()` 把 `Iterator<Item = Result<T, E>>` 收成一个 `Result`,
  第一个 `Err` 立刻短路。
- `if let` 只匹配一个分支,`matches!` 返回布尔值;`while let` 反复匹配直到失败,
  `let...else` 匹配失败即早退,`edition 2024` 的 let 链用 `&&` 串起多个条件与 `let`。
- 用 `thiserror` 的 `#[error]` 定义消息、`#[from]` 自动转换、字段携带上下文、`#[source]` 保留根因。
- mneme 原则:可预期失败返回 `Result`,公开 API 不 panic,绝不静默吞错。

## 动手练习

1. 写 `fn safe_div(a: f32, b: f32) -> Option<f32>`,当 `b == 0.0` 时返回 `None`,否则 `Some(a / b)`;
   用 `?` 在另一个函数里串联两次除法。
2. 给 [03 章练习](03-structs-enums-impl.md)的 `Shape::area` 改成返回 `Result<f32, String>`,半径/边长必须为正。
3. 用 `matches!` 判断一个 `Option<u32>` 是否为 `Some`。
4. 写 `(0..5).map(|n| if n == 3 { Err(n) } else { Ok(n) }).collect::<Result<Vec<_>, _>>()`,
   观察结果,并解释前三个 `Ok` 去了哪里。

## 下一章

[06 泛型与 trait](06-generics-traits.md):一套代码适配多种类型。
