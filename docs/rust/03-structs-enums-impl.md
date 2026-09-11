# 03 结构体、枚举与 impl

> **本章目标**:学会用 `struct` 描述数据、用 `enum` 描述"多选一"、用 `impl` 给类型挂方法,
> 并理解 `#[derive(...)]`、`Default`、`#[non_exhaustive]` 这些 mneme 里随处可见的写法。
> **前置**:[02 章](02-values-and-ownership.md)。
> **对应源码**:[`src/core/types.rs`](../../src/core/types.rs)、[`src/core/metric.rs`](../../src/core/metric.rs)、
> [`src/core/error.rs`](../../src/core/error.rs)、[`src/core/options/`](../../src/core/options)、
> [`src/memory/builder.rs`](../../src/memory/builder.rs)、[`src/memory/pred.rs`](../../src/memory/pred.rs)、
> [`src/index/hnsw.rs`](../../src/index/hnsw.rs)、[`src/index/hidx.rs`](../../src/index/hidx.rs)、
> [`src/query/parse/mod.rs`](../../src/query/parse/mod.rs)。

Rust 没有"类(class)",而是把数据和行为分开:

- **`struct` / `enum` 描述数据**;
- **`impl` 块描述行为**(方法、关联函数);
- **`trait` 描述接口**(下一章 [06](06-generics-traits.md))。

---

## 1. 结构体(struct):三种形态

### 1.1 具名结构体

```rust
pub struct HnswParams {
    pub m: u16,
    pub m0: u16,
    pub ef_construction: u16,
    pub ef_search: u16,
}
```

- 每个字段有名字和类型,`pub` 决定外部能否访问(见 [08 章](08-modules-docs.md))。
- 构造用**结构体字面量**:

```rust
let p = HnswParams { m: 16, m0: 32, ef_construction: 200, ef_search: 64 };
```

见 [`src/core/options/index.rs:8-17`](../../src/core/options/index.rs)。

字段名与局部变量同名时,可以省略 `字段名:` 只写变量名,这叫**字段初始化简写(field init shorthand)**:

```rust
let view = ...;
let blocks = block_count(view);
let ctx = MaskCtx { view, blocks };   // 等价于 MaskCtx { view: view, blocks: blocks }
```

L4 的内部结构几乎都这么构造——`MaskCtx`、`Plan`、`EvalCtx`、`ChannelCtx` 等,见
[`src/query/zmap.rs:20-23`](../../src/query/zmap.rs) 与
[`src/query/plan.rs:80-84`](../../src/query/plan.rs)。简写与完整写法可以混用
(`Plan { candidates, bits, selectivity }` 三个字段全是简写),只影响书写,字段名与语义不变。

只想改几个字段、其余沿用另一份值时,用**结构体更新语法(struct update syntax)** `..`:

```rust
/// hidx 测试使用的一组基准图参数(m=16/m0=32/efc=200/ml=0.5)。
const GRAPH_PARAMS: GraphParams = GraphParams {
    m: 16,
    m0: 32,
    ef_construction: 200,
    ml: 0.5,
};

// 只改 m,其余字段照抄 GRAPH_PARAMS:
let fast = GraphParams { m: 4, ..GRAPH_PARAMS };
```

见 [`src/index/hidx.rs:401-407`](../../src/index/hidx.rs) 与
[`src/index/hidx.rs:519-526`](../../src/index/hidx.rs)。要点:

- `..` 后面的表达式必须与目标类型相同,可以是变量、常量,也可以是 `T::default()`;
- 更新语法按字段依次构造,`..` 表达式里**未被显式覆盖的字段会被移动**进新值;
  本例字段全是 `u16`/`f32`(`Copy`),所以 `GRAPH_PARAMS` 反复使用也不会失效——
  这正是它能当共享常量基底的原因;
- 与 `#[derive(Default)]` 搭配是常见组合:`Config { timeout_ms: 500, ..Config::default() }`。

### 1.2 元组结构体(tuple struct)与 newtype

```rust
pub struct RowId(u64);       // 一个字段,没有字段名
pub struct RelationKind(pub u16);
```

- 用 `.0` 访问第一个字段:`self.0`。
- 只有一个字段的元组结构体就是 **newtype**,用来给底层类型起一个"语义名字"(见 [02 章 §4](02-values-and-ownership.md))。
- 字段前也能加 `pub`(`RelationKind(pub u16)`),允许外部直接读写 `.0`;不加则只能通过方法访问。
  见 [`src/core/options/scoring.rs:90`](../../src/core/options/scoring.rs)。

### 1.3 单元结构体(unit struct)

```rust
pub struct SystemClock;      // 没有任何字段
```

它只表示"存在这样一个类型",用来挂载行为(实现 `Clock` trait),见
[`src/core/options/clock.rs:24`](../../src/core/options/clock.rs)。

---

## 2. impl:给类型挂行为

### 2.1 关联函数(associated function)

不带 `self` 参数,用 `类型::函数名` 调用,常用来做构造器:

```rust
impl RowId {
    pub const fn new(value: u64) -> Self {
        Self(value)          // Self 就是 RowId
    }
}
```

- `Self` 是当前类型的别名。
- `const fn` 表示这个函数可以在编译期求值,也能在运行期调用。
- 调用:`RowId::new(7)`,见 [`src/core/types.rs:28-30`](../../src/core/types.rs)。

#### 2.1.1 `const fn` 能做什么、不能做什么

`const fn` 是"**也能在编译期调用**"的函数,常用于 `new`/`get` 这类无副作用的构造/取值:

```rust
pub const fn new(value: u64) -> Self { Self(value) }
pub const fn needs_norm(&self) -> bool { !matches!(self, Metric::Dot) }
```

它仍能在运行期调用,所以加 `const` 几乎不损失什么。限制在于编译期只能执行**受限子集**:
不能分配堆内存(`String`/`Vec`)、不能 `println!`、不能读时间或文件。因此带校验、返回 `Result`
的 `Dimension::new` 保持普通 `fn`(编译期求值收益不大),而纯取值的 `RowId::get` 可以是 `const`。
见 [`src/core/types.rs:28`](../../src/core/types.rs) 与
[`src/core/options/dimension.rs:40`](../../src/core/options/dimension.rs)。

#### 2.1.2 固有方法还是 trait 方法:`Expr::from_str` 与 `#[allow(clippy::…)]`

L4 给 `Expr` 加了 DSL 解析入口,但**没有**实现标准库的 `FromStr` trait,而是写固有方法:

```rust
impl Expr {
    /// 解析过滤 DSL 字符串。
    ///
    /// # Errors
    /// 语法错误返回 [`MnemeError::FilterParse`],消息携带出错字节位置。
    // 固有方法而非 `FromStr` 实现:宿主 `Expr::from_str(..)` 无需 import trait。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(input: &str) -> Result<Expr> { ... }
}
```

见 [`src/query/parse/mod.rs:19-48`](../../src/query/parse/mod.rs)。要点:

- **固有方法(inherent method)** 直接挂在类型上,`Expr::from_str(..)` 调用时不需要任何 `use`;
  而 **trait 方法**要求 trait 在作用域里才能调用(见 [08 §4](08-modules-docs.md))。
  `FromStr` 是标准库的"从字符串解析"trait,`"42".parse::<i64>()` 走的就是它(见 [06 §3](06-generics-traits.md))。
- clippy 的 `should_implement_trait` lint 会提示"这个 `from_str` 长得像 `FromStr`"。
  mneme 选择保留固有方法,于是用 `#[allow(clippy::should_implement_trait)]` **只关这一条 lint**,
  并在上一行注释写清原因。规范要求:任何 `#[allow(clippy::…)]` 都必须带理由,
  禁止全局 `#![allow(warnings)]`(见 [01 §5](01-toolchain.md))。
- 如果你的类型确实希望被 `str::parse` 通用地调用,就实现 `FromStr`;是否实现是 API 设计取舍,
  不是语法限制。

### 2.2 方法(method)

第一个参数是 `self` / `&self` / `&mut self`:

```rust
impl RowId {
    pub const fn get(self) -> u64 {   // self:值接收(因为 RowId 是 Copy)
        self.0
    }
}

impl Metric {
    pub fn better(&self, x: f32, y: f32) -> bool {  // &self:借用,不夺走所有权
        match self { ... }
    }
}
```

| 接收者 | 含义 | 何时用 |
|---|---|---|
| `self` | 拿走所有权 | 类型是 `Copy`,或方法会消耗该值(如 `into_*`) |
| `&self` | 只读借用 | 绝大多数查询方法 |
| `&mut self` | 可变借用 | 需要修改内部状态(如 `TopK::push`) |

调用:`row_id.get()`、`metric.better(1.0, 0.0)`。

#### 2.2.1 为什么 `RowId::get` 用 `self` 而不是 `&self`?

初学者常觉得"取值方法应该用 `&self`"。但 `RowId` 是 `Copy` 的小类型(只包一个 `u64`),
按值传入等价于复制一个整数,**没有所有权成本**,反而省掉一次间接寻址:

```rust
pub const fn get(self) -> u64 {   // self:值接收;因为 RowId: Copy,调用后原变量仍可用
    self.0
}
```

对比 `Metric::better(&self, ...)`:查询类方法默认用 `&self`,只有"会消耗 `self`"(`into_*`)
或"`self` 很廉价且想省掉借用"时才用值接收。

#### 2.2.2 `mut self`:消费 `self` 的同时允许改它

`TopK::into_sorted_vec(mut self)` 里的 `mut self` 是"**按值接收,且函数体内可改**":

```rust
pub fn into_sorted_vec(mut self) -> Vec<T> {
    self.heap.sort_by(...);       // 消费掉堆,排完序再交出载荷
    self.heap.into_iter().map(|e| e.payload).collect()
}
```

`mut self` 不是一种新的接收者,而是"`self`(值接收) + 一个可变的局部绑定"。调用后原 `TopK`
被**移动**进函数,不能再使用——这正是 `into_*` 命名的语义。见
[`src/core/heap.rs:201-213`](../../src/core/heap.rs)。

#### 2.2.3 链式方法:`mut self -> Self` 与构建者模式

`mut self` 的另一个常见用法是**链式构建**:每个 setter 消费构建器、返回更新后的构建器。

```rust
pub fn dimension(mut self, dimension: u32) -> Self {
    self.dimension = Some(dimension);
    self
}
```

见 [`src/memory/builder/options.rs`](../../src/memory/builder/options.rs)。于是可以一口气写:

```rust
let db = Builder::default()
    .dimension(2)
    .metric(Metric::Cosine)
    .build()
    .unwrap();
```

要点:

- 每个 setter 都是"消费 + 返回",所以链式调用后**原构建器变量不能再使用**(它已被移动)。
- setter 只写字段、不做校验;**校验收敛在 `build()` 一处**,见
  [`src/memory/builder.rs`](../../src/memory/builder.rs)。
- 这是 Rust 里最常用的"可选参数"方案:比 `new(a, b, c, ...)` 好读,也比到处传 `Option` 清晰。

### 2.3 关联常量

```rust
impl Dimension {
    pub const MIN: u32 = 1;
    pub const MAX: u32 = 65_536;
}
```

用 `Dimension::MIN` 访问,见 [`src/core/options/dimension.rs:11-15`](../../src/core/options/dimension.rs)。

---

## 3. 枚举(enum):多选一

Rust 的 `enum` 比 C 的枚举强大得多:**每个变体可以携带不同的数据**。

### 3.1 无数据的枚举

```rust
pub enum Metric {
    Cosine,
    Dot,
    Euclidean,
}
```

见 [`src/core/metric.rs:22-30`](../../src/core/metric.rs)。使用时要 `Metric::Cosine`。

### 3.2 带数据的枚举

```rust
pub enum MnemeError {
    Io(std::io::Error),                       // 元组变体
    DimensionMismatch { expected: u32, got: usize },  // 结构体变体
    Busy(&'static str),                       // 元组变体
}
```

见 [`src/core/error.rs:15-78`](../../src/core/error.rs)。这让你能用**一个类型**表示所有错误,
且每个变体携带自己需要的上下文。

### 3.3 用 `match` 处理枚举

```rust
match metric {
    Metric::Cosine | Metric::Dot => x > y,
    Metric::Euclidean => x < y,
}
```

- `match` 必须**穷尽(exhaustive)**所有变体,否则编译错误——这是 Rust 保证你不漏分支的机制。
- `|` 表示"或",多个模式共用一段代码。
- `_` 是通配符,但 mneme 规范鼓励显式列出,避免新增变体时被静默吞掉。
- 更复杂的模式、`if let`、`matches!` 在 [05 章](05-errors.md) 展开。

分支还能带**守卫(guard)**:写成 `模式 if 条件 => ...`,只有模式匹配**且**条件为真才走该分支。
L4 用它表达"空列表特例":

```rust
match self {
    Expr::And(parts) if parts.is_empty() => json!({ "always": true }),
    Expr::And(parts) => exprs_to_meta(parts),
    ...
}
```

见 [`src/query/json.rs:213-216`](../../src/query/json.rs) 与
[`src/query/display.rs:76-79`](../../src/query/display.rs)。要点:

- 守卫里的 `parts` 已经由模式绑定,可以直接用;多个模式共用同一守卫写成 `A(x) | B(x) if cond`;
- 守卫不是模式的一部分,**穷尽性检查只看主模式**——`Expr::And(parts) if ...` 后面仍需要一个不带守卫的
  `Expr::And(parts)` 分支兜底,否则编译器报 `non-exhaustive patterns`。

### 3.4 递归枚举:为什么 `Expr` 里套着 `Box`

过滤 AST 需要"表达式里包含表达式",所以 `Expr` 是**递归枚举**:

```rust
pub enum Expr {
    Cmp { op: CmpOp, field: String, val: Val },
    ...
    And(Box<[Expr]>),   // 逻辑与:变长子节点列表
    Or(Box<[Expr]>),
    Not(Box<Expr>),     // 逻辑非:单个子节点
    ...
}
```

见 [`src/memory/pred.rs:92-128`](../../src/memory/pred.rs)。如果直接写 `Not(Expr)` /
`And(Vec<Expr>)`,类型大小会**无限大**(`Expr` 里含 `Expr`……循环下去),编译器报
`recursive type has infinite size`。`Box<T>` / `Box<[T]>` 是"堆上单个值"的所有权指针,
本身大小固定(分别为一个指针、一个"指针 + 长度"胖指针),递归因此被截断。要点:

- **构造**:`Expr::Not(Box::new(inner))`、`Expr::And(vec![a, b].into_boxed_slice())`——
  L4 的解析器与 `Display` 都这么造节点,见 [`src/query/parse/mod.rs:228`](../../src/query/parse/mod.rs);
- **解构**:模式写法和普通枚举一样,`Expr::Not(inner)` 里的 `inner` 绑定到 `&Box<Expr>`,
  用 `*inner` 或直接当 `Expr` 用(自动 deref);`match` 里也照常写 `Expr::Not(_)`;
- **列表为什么用 `Box<[Expr]>` 而不是 `Vec<Expr>`**:AST 构造完就不再增删,`Box<[T]>`
  少了 `capacity` 字段、语义上就是"定长",配合 `Vec::into_boxed_slice()` 转换——
  和 [02 §3.7](02-values-and-ownership.md) 的 `Arc<[f32]>` 是同一手法;
- **什么时候需要 `Box`**:递归类型(树、AST、链表)、trait 对象(见 [06 §4](06-generics-traits.md)),
  以及任何"要在类型里放一个自身、又不想让它无限嵌套"的场景。

> `Box` 在 prelude 里,无需 `use`;`Box::new(x)` 把 `x` 移到堆上,所有者离开作用域时
> 连同堆内存一起自动释放(所有权规则,见 [02 §3](02-values-and-ownership.md))。

---

## 4. `#[derive(...)]`:自动生成样板

写在类型上方的 `#[derive(...)]` 是**属性宏**,让编译器自动实现若干 trait:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(u64);
```

| 派生的 trait | 得到的能力 | 什么时候加 |
|---|---|---|
| `Debug` | 能用 `{:?}` 打印,便于调试 | **几乎总是加**(规范要求公共类型必选) |
| `Clone` | `.clone()` | 需要复制时 |
| `Copy` | 赋值即复制,不移动 | 类型小且无资源语义 |
| `PartialEq` / `Eq` | `==` / `!=` | 需要比较相等 |
| `PartialOrd` / `Ord` | `<` `>` 排序 | 需要排序(如 `TopK<T: Ord>`) |
| `Hash` | 能放进 `HashMap` / `HashSet` | 需要哈希 |
| `Default` | `Type::default()` | 有合理默认值 |

> `Copy` 需要 `Clone` 一起派生;`Eq` 需要 `PartialEq`;`Ord` 需要 `PartialOrd` + `Eq`。
> 编译器会提示依赖关系。

### 4.1 为什么有的类型只 `derive(PartialEq)`,不能 `derive(Eq)`/`Ord`?

对照 mneme 的两个类型:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(u64);                       // 字段是 u64 → 可以 Eq/Ord

#[derive(Debug, Clone, PartialEq)]           // 只有 PartialEq
pub struct Scoring {                         // 字段含 f32
    pub w_sim: f32,
    ...
}
```

原因:**`f32`/`f64` 不是 `Eq` 也不是 `Ord`**,只实现了 `PartialEq`/`PartialOrd`。因为浮点有
`NaN`,而 `NaN != NaN`、`NaN.partial_cmp(&x)` 返回 `None`——这违反了 `Eq`(自反)与 `Ord`(全序)的数学要求。

- `derive(Eq)` / `derive(Ord)` 会要求**每个字段**都实现 `Eq` / `Ord`,所以含 `f32` 的
  `Scoring`、`CompactionPolicy`、`Diversity` 只能到 `PartialEq`。
- 这也解释了 `TopK<T: Ord>` 的约束:同分时要按载荷 `a_payload < b_payload` 排序,`<` 来自 `Ord`,
  所以载荷**不能是 `f32`**(`RowId`、`u32` 可以)。
- 需要给 `f32` 排序时,用 `f32::total_cmp`(它定义了一个把 `NaN` 也纳入的全序),而不是
  `partial_cmp().unwrap()`(遇 `NaN` 会 panic)。mneme 的 `TopK` 排序不依赖载荷是浮点,而是由
  `Metric::better` 决定方向,同分再比 `Ord` 载荷。见 [`src/core/heap.rs:201-213`](../../src/core/heap.rs)。

`derive` 也能用在枚举上,见 [`src/core/options/index.rs:66-76`](../../src/core/options/index.rs)
的 `VectorFormat`。

### 4.2 手写比较 trait:字段含 `f32` 又要排序时(L3 的 `Cand`)

`derive` 不是唯一出路。L3 的 HNSW 搜索要维护两个堆:候选前沿按"越近键越大"取最大、
结果堆要随时丢掉最差候选。这要求候选实现完整的 `Ord`(见 [07 §5](07-iterators-closures.md)),
而排序键 `key` 是 `f32`,`derive(Ord)` 用不了(原因见 §4.1)。于是**手写四个 impl**:

```rust
use std::cmp::Ordering;

/// 分数与节点组成的可比较候选(按"越近键越大"排序)。
#[derive(Debug, Clone, Copy)]
struct Cand {
    key: f32,
    score: Score,
    node: u32,
}

impl PartialEq for Cand {
    fn eq(&self, other: &Self) -> bool {
        self.key.total_cmp(&other.key) == Ordering::Equal && self.node == other.node
    }
}

impl Eq for Cand {}

impl PartialOrd for Cand {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Cand {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key
            .total_cmp(&other.key)
            .then(self.node.cmp(&other.node))
    }
}
```

见 [`src/index/hnsw.rs:59-87`](../../src/index/hnsw.rs)。逐条解释:

- **`f32::total_cmp` 提供全序**:`partial_cmp` 遇 `NaN` 返回 `None`,而 `Ord::cmp` 必须
  **永远**给出 `Less`/`Equal`/`Greater` 之一。`total_cmp` 按 IEEE-754 位模式定义了一个
  确定的全序(连 `NaN`、`-0.0` 与 `+0.0` 也有先后),所以能作为 `Ord` 的地基。
- **`impl Eq for Cand {}` 是空实现**:`Eq` 没有任何方法,它只是一句"承诺":`==` 满足自反、
  对称、传递。编译器不允许 `derive(Eq)`(字段含 `f32`),但手写空 impl 表示"我来担保"。
- **`PartialOrd` 必须转发 `Ord`**:契约要求 `partial_cmp(a, b) == Some(cmp(a, b))`,
  两个 trait 对同一对值必须给出一致顺序,标准写法就是 `Some(self.cmp(other))`。
- **`cmp` 末尾的 `.then(...)`**:`Ordering::then` 表示"若前面的比较相等,再用后面的
  比较兜底"。`Ord` 要求全序:两个不同候选不能判为 `Equal`,所以 key 相同的同分节点再按
  `node`(`u32`,本身全序)比较。没有这一步,堆里会出现"`a != b` 但 `a.cmp(b) == Equal`"
  的矛盾状态。
- **`PartialEq` 必须与 `cmp == Equal` 对齐**:即 `a == b` 当且仅当 `a.cmp(b) == Equal`。
  否则 `Ord` 与 `Eq` 互相矛盾,放进 `BinaryHeap`、`sort` 会得到不可预测的结果。

现在 `Cand` 是完整全序类型,可以放进 `BinaryHeap<Cand>`(最大堆)与
`BinaryHeap<Reverse<Cand>>`(最小堆):前者让"最有希望"的候选浮到堆顶,后者让**最差**
候选浮到堆顶方便淘汰。堆的用法见 [07 §5](07-iterators-closures.md)。

> **什么时候手写比较 trait?** ① 字段含 `f32` 但要用于 `sort`/堆/`BTreeMap`;
> ② 需要自定义排序语义(如"按优先级降序、再按时间升序");③ 需要与 `Eq`/`Hash`
> 保持严格一致。手写时永远记住两条一致性规则:`PartialEq` ↔ `Eq` 一致、
> `PartialOrd` ↔ `Ord`(以及 `PartialEq`)一致。

---

## 5. `Default`:默认值

### 5.1 手动实现

```rust
impl Default for HnswParams {
    fn default() -> Self {
        Self { m: 16, m0: 32, ef_construction: 200, ef_search: 64 }
    }
}
```

见 [`src/core/options/index.rs:19-28`](../../src/core/options/index.rs)。

### 5.2 派生 + `#[default]` 标记枚举变体

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorFormat {
    #[default]
    F32,
    F16,
    I8Rescored,
}
```

`#[default]` 指定哪个变体是默认值。见 [`src/core/options/index.rs:67-71`](../../src/core/options/index.rs)。

### 5.3 惯用法

```rust
impl Scoring {
    pub fn new() -> Self {
        Self::default()     // new() 委托给 default(),避免重复写默认值
    }
}
```

见 [`src/core/options/scoring.rs:43-45`](../../src/core/options/scoring.rs)。

---

## 6. 实现标准 trait:`Display`

`Debug`(`{:?}`)适合调试;`Display`(`{}`)适合面向用户的输出,需要手写:

```rust
impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
```

见 [`src/core/types.rs:48-52`](../../src/core/types.rs)。

- `fmt::Formatter<'_>` 里的 `'_` 是**生命周期占位**(见 [04 章](04-borrowing-strings-slices.md))。
- `write!` 宏把格式化的内容写进 `f`,返回 `fmt::Result`。
- 实现 `Display` 后,`.to_string()` 会自动可用。

---

## 7. `#[non_exhaustive]`:为将来留余地

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MnemeError { ... }
```

见 [`src/core/error.rs:16-17`](../../src/core/error.rs)。

`#[non_exhaustive]` 表示"这个枚举将来可能增加变体":
**外部 crate 的代码必须用 `_` 兜底匹配**,不能假设变体已全部列完。
这样 mneme 未来新增错误变体时,不会破坏下游用户的编译。库的公共错误类型通常会加。

---

## 8. 组合:把类型当积木

```rust
pub struct UpdatePatch {
    pub vector: Option<Vec<f32>>,                 // None=不改
    pub text: Option<Option<String>>,             // 外层 None=不改,Some(None)=清空
    pub ttl: Option<Option<Duration>>,            // 同上
    pub valid_time: Option<(i64, Option<i64>)>,   // None=不改;Some((from,to)) 整体替换,to=None 表示无限期
    ...
}
```

见 [`src/core/options/write.rs:45-63`](../../src/core/options/write.rs)。

- `Option<T>` 本身就是一个枚举(`Some(T)` / `None`),见 [05 章](05-errors.md)。
- `Option<Option<T>>` 看似绕,但精确表达了三种语义:**不改 / 清空 / 设为某值**。
- 这种"用类型精确表达状态"的思路,比用布尔标志堆叠(`is_set + is_clear`)更安全,是 mneme 的设计原则之一。

---

## 9. 本章小结

- 数据用 `struct`(具名/元组/单元)和 `enum`(变体可带数据)描述;行为用 `impl` 挂载。
- 结构体字面量支持**字段初始化简写**(`MaskCtx { view, blocks }`);`match` 分支可带**守卫**
  (`模式 if 条件`),但穷尽性只看主模式,带守卫的分支之后仍要兜底。
- 递归 `enum`(如 `Expr`)用 `Box<Expr>`/`Box<[Expr]>` 打断无限大小;固有方法不要求 trait
  在作用域,`#[allow(clippy::…)]` 必须就近写明理由。
- `impl` 里:关联函数不带 `self`,方法带 `self`/`&self`/`&mut self`;还能定义关联常量。
- `#[derive(...)]` 自动生成 `Debug`/`Clone`/`Copy`/比较/`Default` 等;`Default` 也可手写或给枚举变体加 `#[default]`;
  构造时可用结构体更新语法 `..base` 只覆盖部分字段。
- 含 `f32` 的结构体不能 `derive(Eq)`/`Ord`;需要排序时改用 `f32::total_cmp` 手写
  `PartialEq`/`Eq`/`PartialOrd`/`Ord`,并保证 `PartialOrd` 转发 `Ord`、`PartialEq` 与 `cmp == Equal` 一致。
- `match` 必须穷尽所有变体;`Display` 需手写;`#[non_exhaustive]` 给库的公共枚举留演化空间。
- 组合类型(如 `Option<Option<T>>`)能精确表达状态,优于布尔标志。

## 动手练习

1. 在 `examples/hello.rs` 里定义 `struct Point { x: f32, y: f32 }`,派生 `Debug, Clone, Copy`,并打印它。
2. 定义一个 `enum Shape { Circle(f32), Rect { w: f32, h: f32 } }`,写一个 `area(&self)` 方法用 `match` 计算面积。
3. 给 `Point` 实现 `Display`,输出形如 `(1.0, 2.0)`。
4. 去掉 `Point` 的 `PartialEq` 派生,手写 `PartialEq`/`Eq`/`PartialOrd`/`Ord`:先比 `x` 再比 `y`(用
   `f32::total_cmp` 与 `.then(...)`),并建立一个 `Point { x: 1.0, ..p }` 的副本验证比较结果。
5. 定义递归枚举 `enum Tree { Leaf(i32), Node(Box<Tree>, Box<Tree>) }`,写一个 `sum(&self) -> i32`,
   再算 `Node(Leaf(1), Node(Leaf(2), Leaf(3)))` 的和(体会 `Box` 为什么必须有)。

## 下一章

[04 引用、借用、生命周期与字符串](04-borrowing-strings-slices.md):不再移动所有权,而是"借"来看。
