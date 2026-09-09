# 05 错误处理

> **本章目标**:掌握 `Option`、`Result`、`?` 运算符、`match`/`if let`/`matches!`,
> 以及 mneme 用 `thiserror` 定义统一错误枚举的方式。
> **前置**:[03 章](03-structs-enums-impl.md)(枚举与 `impl`)。
> **对应源码**:[`src/core/error.rs`](../../src/core/error.rs)、[`src/core/varint.rs`](../../src/core/varint.rs)、
> [`src/core/options/dimension.rs`](../../src/core/options/dimension.rs)、[`src/core/meta.rs`](../../src/core/meta.rs)。

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

mneme 的 `meta::get_path` 用 `?` 在 `Option` 上做链式短路:

```rust
for segment in path.split('.') {
    current = current.as_object()?.get(segment)?;
}
```

见 [`src/core/meta.rs:42-44`](../../src/core/meta.rs)。任一环返回 `None`,整个函数立即返回 `None`。

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

见 [`src/core/error.rs:81`](../../src/core/error.rs)。于是全库函数签名统一写成 `-> Result<T>`,
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
        Err(MnemeError::Invalid("维度必须在 1..=65536 之间"))
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

见 [`src/core/error.rs:13-34`](../../src/core/error.rs)。

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

## 4. `match`、`if let`、`matches!`

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

---

## 5. panic 还是 Result?mneme 的取舍

| 场景 | 处理方式 |
|---|---|
| 可预期的失败(坏输入、IO、维度不符) | 返回 `Result`,由调用方决定 |
| 编程错误 / 违反不变量(如索引越界) | `panic!`,表示 bug |
| 库代码 | **禁止 panic**,返回结构化错误 |
| 测试代码 | 可以 `unwrap`/`expect`/`panic!` |

mneme 的 L0 契约明确:**公开 API 不 panic**(FC-CORE-INV-002,见
[`tests/core_contracts.rs:136`](../../tests/core_contracts.rs))。甚至浮点的边界也处理成确定值:

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

---

## 7. 本章小结

- `Option<T>` 表示"有/无",`Result<T, E>` 表示"成功/失败",都由编译器强制处理。
- `?` 在 `Result` 或 `Option` 上短路传播;`unwrap`/`expect` 会 panic,生产代码禁用。
- 用 `thiserror` 的 `#[error]` 定义消息、`#[from]` 自动转换、字段携带上下文、`#[source]` 保留根因。
- `if let` 只匹配一个分支,`matches!` 返回布尔值;`match` 必须穷尽。
- mneme 原则:可预期失败返回 `Result`,公开 API 不 panic,绝不静默吞错。

## 动手练习

1. 写 `fn safe_div(a: f32, b: f32) -> Option<f32>`,当 `b == 0.0` 时返回 `None`,否则 `Some(a / b)`;
   用 `?` 在另一个函数里串联两次除法。
2. 给 [03 章练习](03-structs-enums-impl.md)的 `Shape::area` 改成返回 `Result<f32, String>`,半径/边长必须为正。
3. 用 `matches!` 判断一个 `Option<u32>` 是否为 `Some`。

## 下一章

[06 泛型与 trait](06-generics-traits.md):一套代码适配多种类型。
