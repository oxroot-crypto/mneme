# 04 引用、借用、生命周期与字符串

> **本章目标**:理解 `&` / `&mut` 借用规则、切片、`String` / `&str` / `Arc<str>` 的区别,
> 以及生命周期标注到底在标注什么。
> **前置**:[02](02-values-and-ownership.md)、[03](03-structs-enums-impl.md) 章。
> **对应源码**:[`src/core/types.rs`](../../src/core/types.rs)、[`src/core/simd.rs`](../../src/core/simd.rs)、
> [`src/core/meta.rs`](../../src/core/meta.rs)、[`src/core/options/clock.rs`](../../src/core/options/clock.rs)、
> [`src/memory/table/state.rs`](../../src/memory/table/state.rs)。

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

见 [`src/core/simd.rs:33`](../../src/core/simd.rs)。

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

见 [`src/core/meta.rs:37`](../../src/core/meta.rs)。

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

### 4.4 一个实用的记忆法

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

见 [`src/core/heap.rs:107`](../../src/core/heap.rs)。调用方必须先拥有 `let mut top = ...`。

`Clock` trait 的方法用 `&self` 而非 `&mut self`,因为它只是"读时间",不修改自身:

```rust
pub trait Clock: Send + Sync {
    fn now_unix_ms(&self) -> i64;
}
```

见 [`src/core/options/clock.rs:9-16`](../../src/core/options/clock.rs)。

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
- `String` 拥有且可变,`&str` 借用且不可变,`Arc<str>` 拥有且共享、克隆廉价;mneme 的 `Key` 用 `Arc<str>`。
- 生命周期标注只描述引用之间的存活关系,多数情况可省略;`'_` 是占位符。
- `&self` 只读,`&mut self` 可写。

## 动手练习

1. 写一个函数 `fn first<'a>(xs: &'a [u32]) -> Option<&'a u32>`,返回第一个元素。思考 `'a` 能不能省。
2. 在 `examples/hello.rs` 里把 `String` 传给 `&str` 参数(用 `&s`),再用 `.to_string()` 转回 `String`。
3. 试着同时创建两个 `&mut` 引用,读报错,用花括号缩小作用域修好。

## 下一章

[05 错误处理](05-errors.md):`Option`、`Result`、`?` 和 `thiserror`。
