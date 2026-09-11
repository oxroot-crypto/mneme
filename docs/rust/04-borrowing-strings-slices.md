# 04 引用、借用、生命周期与字符串

> **本章目标**:理解 `&` / `&mut` 借用规则、切片、`String` / `&str` / `Arc<str>` 的区别,
> 以及生命周期标注到底在标注什么。
> **前置**:[02](02-values-and-ownership.md)、[03](03-structs-enums-impl.md) 章。
> **对应源码**:[`src/core/types.rs`](../../src/core/types.rs)、[`src/core/simd.rs`](../../src/core/simd.rs)、
> [`src/core/meta.rs`](../../src/core/meta.rs)、[`src/core/options/clock.rs`](../../src/core/options/clock.rs)、
> [`src/memory/table/state.rs`](../../src/memory/table/state.rs)、[`src/index/hnsw.rs`](../../src/index/hnsw.rs)、
> [`src/index/hidx.rs`](../../src/index/hidx.rs)、[`src/index/graph.rs`](../../src/index/graph.rs)、
> [`src/query/parse/`](../../src/query/parse)、[`src/query/iso.rs`](../../src/query/iso.rs)、
> [`src/query/exec.rs`](../../src/query/exec.rs)、[`src/query/display.rs`](../../src/query/display.rs)。

[02 章](02-values-and-ownership.md)说,把值传给函数会**移动所有权**。但大多数时候我们只想"看一眼"
数据,不想把所有权交出去。这就是**借用(borrowing)**:用引用 `&` 借用,用完还回去。

---

## 1. 引用与借用

```rust
fn length_of(s: &str) -> usize {   // 借用,不夺走所有权
    s.len()
}

let s = String::from("hello");
let n = length_of(&s);    // &String 经 Deref coercion 自动变成 &str(见 §2.2)
println!("{s} 的长度是 {n}");   // s 仍然可用
```

- `&T` 是**共享引用(shared reference)**:只读。
- `&mut T` 是**可变引用(mutable reference)**:可读可写。
- 取引用叫**借用(borrow)**,作用域结束后自动归还。

### 1.1 借用规则(编译器强制)

**同一时刻,对同一个值:**

- 要么有**任意多个** `&T`(只读);
- 要么有**恰好一个** `&mut T`(可写);
- 二者不能同时存在。

```rust
let mut v = vec![1, 2, 3];
let r1 = &v;
let r2 = &v;          // 多个只读引用:OK
println!("{r1:?} {r2:?}");

let m = &mut v;
// println!("{r1:?}"); // 错误:已有可变借用,不能再读
m.push(4);
```

这条规则在编译期消灭了**数据竞争(data race)**,是 Rust "无畏并发(fearless concurrency)"的基础。

### 1.2 可变借用的作用域

可变引用是"独占"的,所以不能让两个同时活着:

```rust
let mut x = 1;
let a = &mut x;
let b = &mut x;   // 错误:同时存在两个可变借用
*a += 1;
*b += 1;
```

修法:让借用依次结束(用花括号限定作用域),或干脆直接操作 `x`。

---

## 2. 切片(slice):借用一段连续数据

**切片 `&[T]`** 是对数组或 `Vec` 中一段连续元素的借用,不拥有数据、不复制数据。

```rust
fn dot(a: &[f32], b: &[f32]) -> f32 {
    // a、b 只是"借来看的窗口",可以是数组、Vec、或它们的一部分
    ...
}
```

见 [`src/core/simd.rs:50`](../../src/core/simd.rs)。

- `&[f32]` 可以接收 `&[f32; 4]`(数组的引用)、`&Vec<f32>`、或 `&v[1..3]`(子切片)。
  **函数只依赖"能当切片用",不关心底层是数组还是 Vec**——这是很好的解耦。
- 字符串切片 `&str` 底层是一段字节序列,由类型系统保证其中是合法 UTF-8
  (`String` 的内存布局就是一个字节缓冲区)。

```rust
let arr = [1.0_f32, 2.0, 3.0, 4.0];
let vec = vec![1.0_f32, 2.0];
let s = &arr[1..3];      // &[f32],内容是 [2.0, 3.0]
```

### 2.1 为什么 mneme 的距离函数用 `&[f32]`

`Metric::score(&self, a: &[f32], b: &[f32], ...)` 用切片:
调用方传数组、`Vec`、或子切片都行,且**不会移动或复制向量数据**(向量可能上千维)。
见 [`src/core/metric.rs:57`](../../src/core/metric.rs)。

### 2.2 Deref coercion:为什么 `&String` 能当 `&str` 用

`&[f32]` 能接收数组、`Vec`、子切片,靠的是 Rust 的**解引用强制转换(deref coercion)**:
当实参类型与形参类型不完全一致时,编译器会自动插入若干次 `Deref`/`&` 转换。常见三种:

| 你有的引用 | 形参要 | 自动转成 |
|---|---|---|
| `&String` | `&str` | `&str`(`String: Deref<Target = str>`) |
| `&Vec<T>` | `&[T]` | `&[T]`(`Vec<T>: Deref<Target = [T]>`) |
| `&[T; N]` | `&[T]` | `&[T]`(数组到切片) |

```rust
let s = String::from("hi");
fn takes_str(x: &str) {}
takes_str(&s);                 // &String → &str

let v = vec![1.0_f32, 2.0];
fn takes_slice(x: &[f32]) {}
takes_slice(&v);               // &Vec<f32> → &[f32]

let a = [1.0_f32; 4];
takes_slice(&a);               // &[f32; 4] → &[f32]
```

**要点**:deref coercion 只在**引用层面**发生,不会把 `String` 值本身悄悄转成 `&str`;
`&s` 里的 `&` 是必须的。这也是 mneme 的距离函数签名写成 `&[f32]` 而不是 `&Vec<f32>` 的原因:
调用方传什么容器都行,且不复制数据。

### 2.3 字节数组与切片操作:L3 文件格式的读写基础

L3 的 hidx 文件是"定长头部 + 字节数据区"的二进制格式,全程在 `&[u8]` / `Vec<u8>` 上操作,
需要一组"字节级"惯用法。

**① 字节串字面量与定长数组**:`b"HID1"` 的类型是 `&[u8; 4]`(字节串),前面加 `*` 解引用
得到 `[u8; 4]`,可以当编译期常量:

```rust
/// hidx 魔数。
pub(crate) const MAGIC: [u8; 4] = *b"HID1";

// 切片与数组可以直接比较(标准库提供 [u8] 与 [u8; 4] 的 PartialEq)
if bytes[0..4] != MAGIC {
    return Err(corrupt("魔数不符"));
}
```

见 [`src/index/hidx.rs:20-21`](../../src/index/hidx.rs) 与
[`src/index/hidx.rs:184-186`](../../src/index/hidx.rs)。`bytes[0..4]` 是对切片做范围索引,
得到的是 `[u8]`(不定长);比较运算符会自动借成两边引用,所以 `&[u8]` 与 `&[u8; 4]` 能比。

**② 小端整数与字节互转**:格式规定整数小端存储,`to_le_bytes` 把整数变成 `[u8; N]`,
`from_le_bytes` 反过来:

```rust
header[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
// 解码:
let version = u16::from_le_bytes([bytes[4], bytes[5]]);
```

见 [`src/index/hidx.rs:120`](../../src/index/hidx.rs) 与
[`src/index/hidx.rs:187`](../../src/index/hidx.rs)。

- `slice.copy_from_slice(&src)`:把源切片拷进**已经就位**的目标切片,要求两边长度完全相等,
  否则 panic——长度已知的定长头部用它最合适;
- `Vec::extend_from_slice(&src)`:追加到 `Vec` 末尾、自动增长,用于变长的数据区;
- `Vec::with_capacity(n)`:按预估长度预留容量,避免反复扩容(见
  [`src/index/hidx.rs:73-74`](../../src/index/hidx.rs))。

**③ 空切片与函数指针**:`Graph::neighbors` 在节点/层级越界时返回一个**空切片**而不是
`Option`,让上层循环无须判空:

```rust
pub(crate) fn neighbors(&self, node: u32, level: usize) -> &[u32] {
    self.nodes
        .get(node as usize)
        .and_then(|links| links.neighbors.get(level))
        .map_or(&[][..], Vec::as_slice)
}
```

见 [`src/index/graph.rs:57-62`](../../src/index/graph.rs)。两个细节:

- `&[][..]` 是"空数组的切片",类型是 `&[u32]`(元素类型由 `map_or` 的另一分支推断);
  `[..]` 把"数组引用"显式转成"切片引用",避免某些位置推断不出元素类型。
- `Vec::as_slice` 是一个**函数指针**,签名 `fn(&Vec<T>) -> &[T]` 正好吻合 `map_or` 需要的
  `FnOnce(&Vec<u32>) -> &[u32]`,所以不必写 `|v| v.as_slice()`。凡是签名对得上的关联函数
  都能这样直接当参数传(函数指针不能捕获环境,见 [06 §4.2](06-generics-traits.md))。

> 为什么越界返回空切片、而不是报错?`neighbors` 是搜索热路径上的内部查询:节点 id 来自
> 图自身、层级来自节点,越界只在防御性场景出现。返回空切片让调用方(遍历邻接的循环)自然
> 什么也不做,比每层 `Option` 解包更简洁——这是"边界情况退化到空集"的惯用设计。

### 2.4 字节切片解析惯用法:L4 的 `Parser` 用到的

L4 的 DSL 解析器完全在 `&str` / `&[u8]` 上推进,再用下面这组字面量与切片方法:

```rust
// ① 字节字面量:b'x' 的类型是 u8,不是 char
if byte == b'-' || byte == b'+' { ... }
let digit = i64::from(byte - b'0');   // ASCII 数字转数值

// ② 切片版 strip_prefix:匹配则返回去掉前缀的剩余切片,否则 None
fn strip(bytes: &[u8], expected: u8) -> Option<&[u8]> {
    bytes
        .strip_prefix(&[expected])
        .or_else(|| bytes.strip_prefix(&[expected.to_ascii_lowercase()]))
}

// ③ 整段数字的判定:方法当谓词传(签名对得上)
if bytes.len() < 4 || !bytes[..4].iter().all(u8::is_ascii_digit) { return None; }
```

- `b'0'` / `b'-'` / `b'.'` 是 **ASCII 字节字面量**,类型 `u8`,只能写 ASCII;
  与 `b"..."` 字节串(见 §2.3 ①)配套使用。`byte - b'0'` 就是 C 语言里
  `c - '0'` 的 Rust 写法,前提是已确认 `byte` 是数字。
- `[u8]::strip_prefix` 与 §3.4 要讲的 `str::strip_prefix` 同名同语义,
  只是元素从 `char` 换成 `u8`;`[T]` 与 `str` 都有这组"前缀/后缀"方法。
- `iter().all(u8::is_ascii_digit)`:函数指针当谓词,等价于 `|b| b.is_ascii_digit()`——
  凡是签名能对上就能直接传给迭代器方法(和 §2.3 的 `Vec::as_slice`、[10 §4.6](10-testing.md)
  的 `Cell::get` 是同一个原理)。

见 [`src/query/iso.rs:31-56`](../../src/query/iso.rs) 与 [`src/query/iso.rs:59-63`](../../src/query/iso.rs)。

---

## 3. 字符串家族:`str` / `String` / `&str` / `Arc<str>`

Rust 的字符串是初学者最容易晕的地方。记住两条轴:

1. **拥有还是借用**:`String` / `Arc<str>` 拥有,`&str` 借用。
2. **可变还是不可变**:`String` 可增长,`&str` / `Arc<str>` 不可变。

| 类型 | 拥有? | 可变? | 存储 | 典型用途 |
|---|---|---|---|---|
| `String` | 是 | 是 | 堆,UTF-8 | 需要拼装/修改的字符串 |
| `&str` | 否(借用) | 否 | 指向别处的 UTF-8 | 函数参数、字面量 |
| `Arc<str>` | 是(共享) | 否 | 堆 + 原子引用计数 | 多处共享、克隆廉价 |

- 字符串字面量 `"hello"` 的类型是 `&'static str`(静态生命周期的借用)。
- `String` 可以通过 `&s` 或 `s.as_str()` 变成 `&str`。
- `&str` 通过 `.to_string()` 或 `String::from(...)` 变成 `String`。
- `String` 是可增长的:`.push(c)` 追加一个 `char`、`.push_str(s)` 追加一段 `&str`。
  L4 打印浮点常量时用 `push_str(".0")` 补小数点,解码转义序列时用 `push(char)` 逐字符装配
  (见 [`src/query/display.rs:133-140`](../../src/query/display.rs) 与
  [`src/query/parse/literal.rs:209-218`](../../src/query/parse/literal.rs))。

### 3.1 mneme 的 `Key`:为什么用 `Arc<str>`

```rust
pub struct Key(Arc<str>);

impl Key {
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
```

见 [`src/core/types.rs:220-247`](../../src/core/types.rs)。

- `Arc<str>` 是**原子引用计数的共享字符串**。`Key` 被克隆时只增加引用计数,不复制字符串内容,
  非常适合"同一条记忆的 key 要在多处出现"的场景。
- `Arc` 是线程安全的(`Arc` = Atomically Reference Counted);单线程场景可用 `Rc`,但 mneme 面向并发。
- `impl Into<Arc<str>>` 让 `Key::new("x")` 和 `Key::new(String::from("x"))` 都能用——
  `&str` 和 `String` 都实现了 `Into<Arc<str>>`(见 [06 章](06-generics-traits.md) 的 `impl Trait` 参数)。
- `as_str(&self) -> &str` 返回内部字符串的借用,生命周期绑定到 `&self`。

### 3.2 `From` 实现:从多种来源构造

```rust
impl From<&str> for Key {
    fn from(value: &str) -> Self { Self(Arc::from(value)) }
}
impl From<String> for Key {
    fn from(value: String) -> Self { Self(Arc::from(value)) }
}
```

见 [`src/core/types.rs:249-259`](../../src/core/types.rs)。有了 `From`,就能用 `Key::from("x")` 或
`let k: Key = "x".into();`。

### 3.3 深入:`str` 是不定长类型,`&str`/`Arc<str>` 是"胖指针"

`str` 本身没有编译期已知的大小(长度取决于运行期内容),叫**不定长类型(unsized type)**。
你几乎不会直接持有 `str`,只持有指向它的**胖指针(fat pointer)**——一个指针 + 一个长度:

| 类型 | 内存布局(概念上) | 能否改内容 |
|---|---|---|
| `String` | (ptr, len, capacity) | 可增长/修改 |
| `&str` | (ptr, len),指向别处 | 只读 |
| `Arc<str>` | (ptr, len) + 原子引用计数 | 只读,可共享 |

因此:

- `"hello"` 是 `&'static str`——一个指向程序只读数据段的胖指针。
- `Arc<str>` 的 `clone` 只把引用计数 +1,**不复制字符**;但 `Arc::from("x")`、`Key::new("x")`
  会**分配**一个新堆缓冲区并把字符拷进去。所以"`Key` 克隆廉价"说的是 `clone()`,不是构造。
- `String → Arc<str>` 可以零拷贝完成(`Arc::from(string)` 直接接管缓冲区);`&str → Arc<str>`
  必须拷贝一次。

见 [`src/core/types.rs:219-259`](../../src/core/types.rs)。

**`.as_ref()` 借出 `&str`:`AsRef` trait。** 除了 `&*arc`(解引用)和 `Key::as_str()`,
还可以用 `.as_ref()`——它来自标准库的 **`AsRef`** trait(含义是"把 `&self` 转成另一种引用",
零成本)。`Arc<str>`、`String` 都实现了 `AsRef<str>`,所以 JSON 编码 `Val::Str` 里的 `Arc<str>`
时直接写 `value.as_ref()` 就得到 `&str`,见 [`src/query/json.rs:90`](../../src/query/json.rs)。
注意 `Option<T>` 也有一个同名的**固有方法** `as_ref()`(见 [05 §1.2](05-errors.md)),
两者同名不同源,按接收者是不是 `Option` 区分。

**`Arc<str>` 能直接和 `str` 比较。** 标准库提供了跨类型 `PartialEq`,所以比较字符串内容时不必
先转成 `String`、也不依赖 `Arc` 指针地址。L4 查命名空间就利用了这点:

```rust
fn resolve_ns_id(&self, view: &ReaderView) -> Option<NsId> {
    view.ns_registry.iter().find_map(|(id, path)| {
        if **path == *self.ns_path {
            Some(*id)
        } else {
            None
        }
    })
}
```

见 [`src/query/exec.rs:168-176`](../../src/query/exec.rs)。`ns_registry` 的值类型是 `Arc<str>`,
迭代给出 `path: &Arc<str>`,所以 `**path` 一路解到 `str`;`self.ns_path: Arc<str>`,`*self.ns_path`
同样解到 `str`——两边比较的是字符内容。测试里 `**path == *"n"` 的 `*"n"` 也是把字面量 `&str`
解一层得到 `str`(见 [`src/query/plan.rs:114-117`](../../src/query/plan.rs))。

### 3.4 原始字符串与"模式" API:L4 解析器的字符串日常

L4 的过滤 DSL 本身含双引号(如 `kind == "preference"`),所以测试与 doctest 里满是
**原始字符串(raw string)**:

```rust
r#"kind == "preference" && importance > 0.5"#   // 引号不用转义
```

- `r"..."` 里的 `\` 不再是转义符;字符串内含 `"` 时改用 `r#"..."#`(井号可叠加),
  直到 `"#` 才结束。DSL 测试因此省去一堆 `\"`。
- 普通字符串里的转义(`\"`、`\\`、`\n`、`\t`)要手写代码解码——L4 的 `parse_quoted`
  就逐个处理这些序列(见 [`src/query/parse/literal.rs:188-226`](../../src/query/parse/literal.rs));
  原始字符串省掉的正是这层。

字符串查找方法接受的不只是 `&str`,而是各种**模式(pattern)**:`&str`、`char`、
`char` 数组、以及 `fn(char) -> bool` 的闭包/函数指针:

```rust
after.starts_with(|c: char| c.is_alphanumeric() || c == '_')   // 闭包当模式
text.contains(['.', 'e', 'E'])                                  // 字符数组当模式
trimmed.starts_with(')')
rest.strip_prefix(keyword)                                      // Option<&str>,不是"剩余切片"
```

见 [`src/query/parse/mod.rs:110-121`](../../src/query/parse/mod.rs) 与
[`src/query/display.rs:133-140`](../../src/query/display.rs)。常用成员:
`starts_with`/`ends_with`/`contains`/`find`/`split`/`trim_start_matches` 等。
不带模式的 `trim_start()` / `trim_end()` / `trim()` 则去掉开头 / 结尾 / 两端的全部 Unicode 空白
(与 `trim_start_matches` 要传模式不同);L4 解析器用它跳过关键字后面的空格,如
`after.trim_start().starts_with('(')`,见 [`src/query/parse/mod.rs:123-133`](../../src/query/parse/mod.rs)。

> `str::strip_prefix` 返回 `Option<&str>`:匹配时是"去掉前缀后的借用",不匹配是 `None`,
> 所以常配 `let ... else`(见 [05 §4.6](05-errors.md))做"匹配失败就早退"。L4 解析器
> 用它判定 `true`/`now`/`ts`/`exists` 等关键字,既完成比较又顺手吃掉输入
> (见 [`src/query/parse/mod.rs:111-121`](../../src/query/parse/mod.rs))。

---

## 4. 生命周期(lifetime):引用能活多久

生命周期是**编译器用来追踪"引用不会比它指向的数据活得更久"的标注**。
大多数时候你**不用写**——编译器能推断(生命周期省略规则)。只有编译器无法推断时,才需要你标注。

### 4.1 一个需要标注的例子

```rust
pub fn get_path<'v>(value: &'v Meta, path: &str) -> Option<&'v Meta> {
    ...
}
```

见 [`src/core/meta.rs:39`](../../src/core/meta.rs)。

- `'v` 读作"生命周期 v",是一个**泛型参数**,但泛化的是"存活时间"而不是类型。
- 签名含义:**返回的引用活得和 `value` 的引用一样久**;`path` 的生命周期无关紧要。
- 编译器无法自己判断返回的 `&Meta` 是来自 `value` 还是 `path`(两个都是引用),所以必须显式说明。

### 4.2 省略规则:编译器替你补生命周期

生命周期省略(elision)有三条规则,按顺序套用:

1. **每个输入引用各自获得一个生命周期参数**。`fn f(a: &T, b: &T)` 变成 `fn f<'a, 'b>(a: &'a T, b: &'b T)`。
2. **如果恰好只有一个输入生命周期,那么输出引用就用它**。
3. **如果输入里有 `&self` / `&mut self`,那么输出引用就用 `self` 的生命周期**(规则 3 优先于规则 2)。

所以 `fn as_str(&self) -> &str` 被补成 `fn as_str<'a>(&'a self) -> &'a str`:只有一个输入(规则 1),
且是 `&self`(规则 3)。而 `get_path<'v>(value: &'v Meta, path: &str) -> Option<&'v Meta>` 有**两个**
输入引用、又不是方法,规则 2/3 都不适用,编译器无法确定输出借谁,所以必须手写 `'v`。

### 4.3 `'_` 占位符

```rust
fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
```

`'_` 表示"这里有个生命周期,但我不关心它叫什么,交给编译器"。见
[`src/core/types.rs:49`](../../src/core/types.rs)。

### 4.4 结构体也能借用:`QueryRef<'a>`

引用不只出现在函数参数里,结构体字段也可以存引用——这时结构体必须带生命周期参数:

```rust
/// 查询向量及其预计算范数平方;打包传参以避免在层搜索接口上堆叠参数。
#[derive(Debug, Clone, Copy)]
pub(crate) struct QueryRef<'a> {
    /// 查询向量。
    pub(crate) vector: &'a [f32],
    /// 查询向量范数平方(度量需要时)。
    pub(crate) norm_sq: f32,
}
```

见 [`src/index/hnsw.rs:27-34`](../../src/index/hnsw.rs)。含义:

- `QueryRef<'a>` 读作"借用寿命为 `'a` 的查询视图";`<'a>` 虽是类型参数,泛化的却是生命周期;
- 它**不拥有**向量(字段类型是 `&'a [f32]`),所以实例不能活得比被借用的向量久——编译器保证;
- 函数签名里用 `QueryRef<'_>`(省略具体名字)或 `QueryRef<'a>` 都行:
  `search_layer(&self, query: QueryRef<'_>, ...)`,见
  [`src/index/hnsw.rs:224-230`](../../src/index/hnsw.rs);
- 字段都是 `Copy`(引用与 `f32` 都 `Copy`),所以 `QueryRef` 自己也能 `derive(Copy)`,
  按值传递十分廉价——它像一个"带数据的借用凭证"。

> 为什么要打包成结构体?层搜索接口本来要分别传 `vector` 和 `norm_sq` 两个参数,打包后
> 只传一个;查询期间字段不变,按值复制一份即可,不必反复向上层借用。这是"用类型把相关
> 数据绑在一起"的小例子。

### 4.5 方法返回借"输入"而不是 `&self` 的引用:解析器的 `rest`

L4 的递归下降解析器一边读字符串一边"报字节位置",于是把输入存进结构体:

```rust
struct Parser<'a> {
    input: &'a str,
    pos: usize,
    now_ms: i64,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.input[self.pos..]
    }
}
```

见 [`src/query/parse/mod.rs:61-82`](../../src/query/parse/mod.rs)。三个新知识点:

- **`impl<'a> Parser<'a>`**:为带生命周期参数的类型实现方法时,`impl` 块也要引入 `'a`;
  如果方法不返回 `'a`,也可以写 `impl Parser<'_>` 省去命名(见
  [`src/query/parse/literal.rs:29`](../../src/query/parse/literal.rs))。
- **为什么返回 `&'a str` 而不是省写**:按 §4.2 的省略规则 3,方法返回的引用若省略生命周期,
  会被绑到 `&self`——意思是"解析器还活着,切片才有效"。但这里想要的不是那样:`rest()`
  返回的切片指向**构造解析器时的输入字符串**(字段 `input: &'a str`),与这一次 `&self`
  借用的长短无关;解析器本身随后被丢掉,切片仍应可用。所以必须显式写 `&'a str`。
- **对比 `Key::as_str(&self) -> &str`**:它返回的确实指向 `self` 内部,省略规则给出
  `&'a self` 是正解。要不要显式标注,取决于"返回值借的是**字段**还是 `self`"。

> 这也解释了 `parse_at` 为什么可以建一个临时 `Parser`、拿完结果就丢:AST 里的
> `String` / `Arc<str>` 都是拥有型(见 §3),不借解析器;真正借用输入的返回值都被
> 限制在 `'a` 之内(见 [`src/query/parse/mod.rs:50-59`](../../src/query/parse/mod.rs))。

### 4.6 一个实用的记忆法

> 生命周期标注**不改变任何运行时代码**,它只是向编译器"承诺"引用之间的关系。
> 如果承诺错了,编译失败,而不是运行时出错。

---

## 5. `&self`、`&mut self` 与内部可变性

`&self` 是只读借用。如果方法需要改内部状态,就用 `&mut self`:

```rust
impl<T: Ord> TopK<T> {
    pub fn push(&mut self, score: Score, payload: T) { ... }
}
```

见 [`src/core/heap.rs:131`](../../src/core/heap.rs)。调用方必须先拥有 `let mut top = ...`。

`Clock` trait 的方法用 `&self` 而非 `&mut self`,因为它只是"读时间",不修改自身:

```rust
pub trait Clock: Send + Sync {
    fn now_unix_ms(&self) -> i64;
}
```

见 [`src/core/options/clock.rs:10-19`](../../src/core/options/clock.rs)。

> **那"用 `&self` 却要改内部状态"怎么办?** 这就是**内部可变性(interior mutability)**:
> 用 `Cell`/`RefCell`(单线程)或 `Mutex`/`RwLock`/原子类型(多线程)把"可变性"藏进类型内部,
> 让 `&self` 也能改。`Clock` 本身不改状态,所以用 `&self`;而像缓存、计数器这类共享状态才会用到
> 内部可变性。注意:`RefCell` 不是 `Sync`,不能跨线程;跨线程共享要用 `Mutex` 或原子类型。

### 5.1 锁与守卫:L1 怎么把可变状态藏进 `&self`

L1 的 `Table` 正是"用 `&self` 改内部状态"的实例:

```rust
pub(crate) struct Table {
    pub(crate) writer: Mutex<WriterState>,
    pub(crate) reader: RwLock<Arc<ReaderView>>,
    pub(crate) config: Arc<Config>,
}
```

见 [`src/memory/table/state.rs`](../../src/memory/table/state.rs)。

- `Mutex<T>`(互斥锁)保证同一时刻只有一个写者;`RwLock<T>`(读写锁)允许多个读者**并发**、
  写者独占。写路径慢且要串行,读路径要尽可能并发——所以各用一把合适的锁。
- `.lock()` / `.read()` / `.write()` 返回**守卫(guard)**:`MutexGuard<'_, T>`、
  `RwLockReadGuard<'_, T>`。守卫像智能指针一样实现 `Deref`/`DerefMut`,用起来和 `&T`/`&mut T`
  差不多;它离开作用域时**自动解锁**(RAII),不需要手写 unlock。
- 守卫的生命周期绑定到 `&self`,因此借用检查器会阻止你把锁"忘了放"或把里面数据带出锁外。

```rust
pub(crate) fn write(&self) -> MutexGuard<'_, WriterState> {
    self.writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
```

见 [`src/memory/table/state.rs`](../../src/memory/table/state.rs)。

- `lock()` 返回 `LockResult`:持锁线程 panic 会让锁**中毒(poisoned)**,后续 `lock()` 得到 `Err`。
  mneme 的选择是**恢复数据继续**(不让一次 panic 永久废掉整库),所以用
  `unwrap_or_else(|poisoned| poisoned.into_inner())` 取出内部值,而不是 `unwrap()` 再 panic。
- 推论:持锁期间尽量只做必要工作。L1 的读路径克隆一个 `Arc<ReaderView>` 后**立刻释放读锁**,
  真正的扫描在锁外进行(见 [02 §3.7](02-values-and-ownership.md))。

### 5.2 原子类型:不用锁的共享计数器

L4 需要一个"全局查询 id 分配器":每次 `execute()` 没显式给 `QueryId`,就取一个**不重复**的
新编号,跨线程也要成立。用锁太重,标准库给了原子类型:

```rust
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_QUERY_ID: AtomicU64 = AtomicU64::new(1);

let id = NEXT_QUERY_ID.fetch_add(1, Ordering::Relaxed);
```

见 [`src/query/exec.rs:35`](../../src/query/exec.rs) 与
[`src/query/exec.rs:262-265`](../../src/query/exec.rs)。要点:

- `static` 是**整个程序唯一**的变量(比 `const` 多一个固定地址);普通 `static mut` 的读写
  是 `unsafe`,而 `AtomicU64` 提供安全的原子读写,`&self` 也能改内部值——这是它版本的
  "内部可变性"。
- `fetch_add(1, ...)` 原子地"返回旧值,再加 1":两个线程同时调用会拿到不同旧值,不会重号。
  `load`(读)、`store`(写)、`compare_exchange`(比较并交换)是另外几个常用操作。
- `Ordering::Relaxed` 只保证**这个操作自身**的原子性,不建立跨线程的先后可见性;
  计数器只要求"不重号",所以够用。真正的"一个线程写、另一个线程必须看到"要用
  `Acquire`/`Release`/`SeqCst`,并写注释说明理由。
- **与锁的取舍**:单个整数用原子(无阻塞、无守卫、无中毒问题);一组相关状态仍用
  `Mutex`/`RwLock`(§5.1)——原子一次只保护"一个值"。
- 测试里的**假时钟**也是原子:`struct FakeClock(AtomicI64)` 用 `load`/`store` 在 `&self`
  下推进时间,既满足 `Clock` 签名,又能注入并发测试(见
  [`tests/l4_contracts.rs:31-38`](../../tests/l4_contracts.rs))。

---

## 6. 你会遇到的编译器报错

| 报错关键词 | 原因 | 修法 |
|---|---|---|
| `cannot borrow X as mutable, as it is not declared as mutable` | 想借可变的变量没加 `mut` | 声明时加 `mut` |
| `cannot borrow X as mutable more than once` | 同时两个 `&mut` | 缩小作用域 |
| `cannot borrow X as immutable because it is also borrowed as mutable` | 读写冲突 | 先结束可变借用 |
| `X does not live long enough` | 引用比数据活得久 | 让数据活更久,或返回拥有型(如 `String`) |
| `missing lifetime specifier` | 编译器无法推断 | 加 `<'a>` 标注 |

---

## 7. 本章小结

- `&T` 共享引用(可多个)、`&mut T` 可变引用(唯一),二者互斥;借用规则在编译期消灭数据竞争。
- `&[T]` 是切片借用,可接收数组、`Vec`、子切片;mneme 的距离函数因此不复制向量。
- 字节级操作:`*b"..."` 是定长字节数组;`to_le_bytes`/`from_le_bytes` 做整数与字节互转;
  `copy_from_slice` 要求等长,`extend_from_slice` 自动增长;空切片可作"退化结果"。
- 字节字面量 `b'0'` 是 `u8`;`[u8]::strip_prefix` 返回 `Option<&[u8]>`,常配合 `let...else` 早退。
- 原始字符串 `r#"..."#` 免转义;`str` 的前缀/子串方法接受字符、闭包、字符数组等"模式"。
- `String` 拥有且可变,`&str` 借用且不可变,`Arc<str>` 拥有且共享、克隆廉价;mneme 的 `Key` 用 `Arc<str>`。
- `String` 用 `push`/`push_str` 增长;从 `Arc<str>`/`String` 借 `&str` 可用 `.as_ref()`(`AsRef` trait);
  `Arc<str>` 与 `str`/`&str` 能直接 `==`,比较的是内容而不是指针。
- 生命周期标注只描述引用之间的存活关系,多数情况可省略;`'_` 是占位符;
  结构体字段存引用时,结构体自身要带生命周期参数(如 `QueryRef<'a>`)。
- 方法返回借"字段"的引用时要显式写字段的生命周期(`fn rest(&self) -> &'a str`);
  `impl<'a> Parser<'a>` 为带生命周期参数的类型实现方法。
- `&self` 只读,`&mut self` 可写;`static` + `AtomicU64` 则是无锁的共享计数器
  (`fetch_add` 返回旧值,`Ordering` 决定可见性强度)。

## 动手练习

1. 写一个函数 `fn first<'a>(xs: &'a [u32]) -> Option<&'a u32>`,返回第一个元素。思考 `'a` 能不能省。
2. 在 `examples/hello.rs` 里把 `String` 传给 `&str` 参数(用 `&s`),再用 `.to_string()` 转回 `String`。
3. 试着同时创建两个 `&mut` 引用,读报错,用花括号缩小作用域修好。
4. 定义 `struct Word<'a> { text: &'a str }`,实现 `fn tail(&self) -> &'a str`(返回去掉首字符的切片),
   并解释返回类型为什么不能省写 `'a`。

## 下一章

[05 错误处理](05-errors.md):`Option`、`Result`、`?` 和 `thiserror`。
