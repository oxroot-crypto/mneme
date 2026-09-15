# 02 值、类型与所有权

> **本章目标**:掌握 Rust 的变量、基本类型,以及最重要的**所有权(ownership)**。
> **前置**:读过 [01 章](01-toolchain.md),会 `cargo build`。
> **对应源码**:[`src/core/types.rs`](../../src/core/types.rs)、[`src/core/varint.rs`](../../src/core/varint.rs)、
> [`src/core/metric.rs`](../../src/core/metric.rs)、[`src/memory/table/state/`](../../src/memory/table/state/)、
> [`src/index/hidx/`](../../src/index/hidx/)、[`src/index/filtered.rs`](../../src/index/filtered.rs)、
> [`src/query/iso.rs`](../../src/query/iso.rs)、[`src/query/zmap.rs`](../../src/query/zmap.rs)、
> [`src/query/fusion.rs`](../../src/query/fusion.rs)、[`src/query/parse/`](../../src/query/parse/)、
> [`src/quant/scalar_i8.rs`](../../src/quant/scalar_i8.rs)、[`src/quant/f16.rs`](../../src/quant/f16.rs)。

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
mneme 的 varint 解码显式检查溢出,见 [`src/core/varint.rs:103`](../../src/core/varint.rs)。

#### 2.1.2 不丢数据的转换:`TryFrom` 与受检运算

`as` 的危险在于"悄悄丢数据"。当不允许丢数据时,标准库给了三种工具:

```rust
// ① TryFrom:可能失败的转换,返回 Result
let len: usize = 70_000;
let too_big = u16::try_from(len);       // Err(TryFromIntError) —— 超出 u16 范围
let ok = u16::try_from(300_usize);      // Ok(300)

// ② checked_*:可能溢出的算术,返回 Option
let big = usize::MAX;
big.checked_add(1);                     // None
4_usize.checked_mul(big);               // None

// ③ saturating_*:溢出时"夹住"到边界,绝不回绕
4_usize.saturating_mul(big);            // usize::MAX
```

L3 的 hidx 编码要保证"长度字段必须塞得进 `u32`"(`put_u32` 的入参就是 `u32`):

```rust
put_u32(
    &mut node_table,
    u32::try_from(adj_blob.len())
        .map_err(|_| encode_too_large("hidx 邻接区字节数", adj_blob.len()))?,
);
```

见 [`src/index/hidx/encode.rs:43-47`](../../src/index/hidx/)。`try_from` 失败返回 `Err`,配合
`.map_err(...)?` 转成带字段名与实际值的领域错误——**绝不静默截断**。
解码路径同理:`validate_layout` 用 `checked_mul`/`checked_add` 防止恶意长度在偏移计算时回绕,
溢出就用 `ok_or_else` 变成结构化错误:

```rust
let expected_node_table = header
    .count
    .checked_mul(NODE_TABLE_ENTRY)
    .ok_or_else(|| corrupt("node_table_len 溢出"))?;
```

见 [`src/index/hidx/read.rs:115-118`](../../src/index/hidx/read.rs),`ok_or_else` 的用法在
[05 §1.2](05-errors.md) 展开。

选择原则:

- `From`/`Into`:转换**永远成功**(如 `u32 → u64`),用最自然的写法;
- `TryFrom`:转换**可能失败**(窄化、解析),必须显式处理 `Err`;
- `checked_*`:算术**可能溢出**,返回 `Option`,适合"溢出即非法输入";
- `saturating_*`:算术溢出时**取边界值**,适合"宁可夹紧不可回绕"——过滤档②的
  `ef` 放大就用了 `base.saturating_mul(AMPLIFIED_TIER_FACTOR)`,见
  [`src/index/filtered.rs:163`](../../src/index/filtered.rs)。

#### 2.1.3 L4 用到的几个整数方法

```rust
// ① 欧几里得除法:向负无穷取整,余数永远非负
let ms = -1_i64;
ms / 86_400_000;            // 0:截断除法,向 0 取整
ms.div_euclid(86_400_000);  // -1:floor 除法
ms.rem_euclid(86_400_000);  // 86_399_999:非负余数

// ② 向上取整的除法:块数 = ceil(元素数 / 每块行数)
view.slots.len().div_ceil(ZONE_BLOCK_ROWS);

// ③ 无符号绝对值:对 i64::MIN 取负会溢出,unsigned_abs 不会
value.unsigned_abs() <= MAX_EXACT_INT as u64;
```

- 为什么需要 `div_euclid`:ISO 8601 格式化要把 Unix 毫秒拆成"天数 + 当日余量"。
  1970 年之前的时间戳是负数,`/` 向 0 取整会把 `-1 ms` 算成第 0 天,时间直接错一天;
  `div_euclid`/`rem_euclid` 才是日历需要的 floor 语义。
- 为什么用 `div_ceil`:`a.div_ceil(b)` 就是「`(a + b - 1) / b`」,但不会在 `a == 0`
  或大数相加时出幺蛾子;数 zone map 的块数正好是「向上取整」。
- 为什么用 `unsigned_abs`:对 `i64::MIN` 直接取负会溢出 panic,`unsigned_abs`
  返回 `u64`,先比上限再转 `f64`(见 §2.2 的 2^53)。

见 [`src/query/iso.rs:192-193`](../../src/query/iso.rs)、
[`src/query/zmap.rs:27-30`](../../src/query/zmap.rs) 与
[`src/query/zmap.rs:179-182`](../../src/query/zmap.rs)。

L6 的 f16 解码又用到 `usize::is_multiple_of`:

```rust
if !codes.len().is_multiple_of(BYTES_PER_ELEMENT) {
    return Err(MnemeError::Corrupted { ... });   // 码流长度不是 2 的整数倍
}
```

见 [`src/quant/f16.rs:27`](../../src/quant/f16.rs)。`n.is_multiple_of(m)` 判断 `n` 能否被 `m`
整除,等价于 `n % m == 0`,但 `m == 0` 时不会 panic(仅当 `n == 0` 返回 `true`),
意图也比"取模再判 0"更直白。

### 2.2 浮点

- `f32`(单精度,约 7 位有效数字)、`f64`(双精度)。
- mneme 的向量分量与分数统一用 `f32`(见 [`src/core/metric.rs:16`](../../src/core/metric.rs) 的
  `pub type Score = f32;`),因为向量计算量大、`f32` 带宽和速度更优。
- 浮点数比较不要用 `==`,要用误差阈值:

```rust
let s = 0.1_f32 + 0.2;
assert!((s - 0.3).abs() < 1e-6);   // 惯用写法
```

mneme 的测试里到处是 `(x - expected).abs() < 1e-6`,见 [`src/core/metric.rs:250-267`](../../src/core/metric.rs)。

浮点还有几个 mneme 常用的"防 NaN / 防失控"工具:

```rust
let ml = 0.5_f32;
ml.is_finite();            // true;NaN、±inf 都是 false

let amp = 2.5_f32;
amp.clamp(1.0, 8.0);       // 2.5;把值夹到 [1.0, 8.0] 闭区间

f32::MIN_POSITIVE;         // 最小的正规格化浮点数,常用来替代 0 做除数下限
```

- `is_finite` 用于**入口校验**:hidx 头部解码到 `ml` 时,先 `!ml.is_finite() || ml <= 0.0`
  就拒绝,绝不让 `NaN` 混进建图参数(见 [`src/index/hidx/read.rs:107`](../../src/index/hidx/read.rs))。
- `clamp(min, max)` 同时完成上下限约束;过滤档①的 `ef` 放大系数写作
  `(1.0 / selectivity.max(f32::MIN_POSITIVE)).clamp(1.0, MAX_EF_AMPLIFICATION)`,
  既不除以 0 也不超放大上限(见 [`src/index/filtered.rs:160`](../../src/index/filtered.rs))。
- `MIN_POSITIVE` 是**下限哨兵**:把可能为 0 的分母抬到最小正数,结果虽大但有限,
  比 `0.0` 分母产生的 `inf`/`NaN` 更好处理。

> `clamp` 要求 `min <= max` 且两者非 `NaN`,否则 panic;值本身是 `NaN` 时返回 `NaN`,
> 所以**先 `is_finite` 校验、再 `clamp`** 是安全顺序。

L4 又用到几个浮点常量与精度事实:

```rust
// ① 极值哨兵:做 min / max 归约时当"初始累加器"
let min = values.iter().copied().fold(f64::INFINITY, f64::min);
let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);

// ② 非有限值的三种形态:NaN、+∞、-∞
alpha.is_finite();        // 三者都是 false;配合 [0,1] 区间判定拒绝非法参数

// ③ f64 只能精确表示 |整数| ≤ 2^53(约 9.0e15)
9_007_199_254_740_992_i64 as f64;   // 2^53,精确
9_007_199_254_740_993_i64 as f64;   // 2^53 + 1,转 f64 后与 2^53 相等(静默舍入)
```

- `f64::INFINITY` / `NEG_INFINITY`(以及 `f32` 的同名版本)是关联常量,分别表示 ±∞;
  融合做 min-max 归一化时用它们当 `fold` 初值(`f64::min(+∞, x) == x`,任何有限值都能顶掉初值)。
  这里刻意用 `f64`:两个 `f32` 极端值相减得到的极差会溢出成 `inf`,升位再算才能避免 `inf/inf = NaN`。
- `f32::EPSILON` 是最小的"使 `1.0 + ε != 1.0`"的正数,测试里用它造"极差极小但不为 0"
  的输入,验证归一化不会把极小差异当成单点。
- **2^53 精度上限**是 L4 计划器的关键约束:zone map 的区间比较要把整数转成 `f64`,
  超过 2^53 的 `i64` 转完会失真,可能被误判"不可能命中"而错误剪枝。所以
  `exact_int` 先检查 `unsigned_abs() <= MAX_EXACT_INT`,超了就返回 `None`,
  由调用方退回"全 1 位图"(无法判断就不剪)。

见 [`src/query/fusion.rs:86-87`](../../src/query/fusion.rs)、
[`src/query/fusion.rs:202-221`](../../src/query/fusion.rs) 与
[`src/query/zmap.rs:166-182`](../../src/query/zmap.rs)。

L6 的量化编解码又用到两个浮点方法:

```rust
// ① round:四舍五入到最接近的整数(仍是 f32);i8 编码把连续值折到 [0, 255]
let code = ((value - params.min(dim)) / delta).round();
code.clamp(0.0, MAX_CODE) as u8;

// ② powi:整数次幂;测试用它与契约里的相对精度 2^-11 一一对应
let bound = original.abs() * 2.0_f32.powi(-11) + 1e-6;
```

见 [`src/quant/scalar_i8.rs:123`](../../src/quant/scalar_i8.rs) 与
[`src/quant/f16.rs:75`](../../src/quant/f16.rs)。要点:

- `round` 是"舍入到最近整数,`.5` 向远离 0 的方向",不是截断(`trunc`)也不是向下取整
  (`floor`);`as u8` 只负责最后一步截位(见 §2.1.1 的截断规则)。
- `powi` 的指数是 `i32`,负指数表示倒数;它比 `powf` 快且不引入对数误差,适合"2 的整数次幂"
  这类常量。f16 测试里 `powi(-11)` 就是把文档里的 `2^-11` 直接写进代码,
  免得读者对 `1.0 / 2048.0` 猜半天。

### 2.3 半精度浮点 `half::f16`

L6 的量化副本还有一个格式:**f16(半精度浮点)**。为什么不用 `f32` 直接存?因为向量的查询瓶颈
在**内存带宽**:每个分量从 4 字节降到 2 字节,同一条带宽能多喂一倍的候选(见设计
[08 L6 §1](../design/08-l6-quant.md))。i8 虽然更省(1 字节),但需要段级 `(v_min, v_max)`
参数表、天然不对称;f16 自带 IEEE-754 的指数与符号,无参数表、误差无偏,是"不想维护刻度表"
时的保守选择。

Rust 标准库没有半精度类型,mneme 经 feature `quant-f16` 引入外部 crate `half`
(见 [`Cargo.toml:37-38`](../../Cargo.toml)),用它的 `half::f16`:

```rust
use half::f16;

/// 单分量字节数。
pub(crate) const BYTES_PER_ELEMENT: usize = 2;

/// 单行编码为小端 f16 码流。
pub(crate) fn encode_row(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * BYTES_PER_ELEMENT);
    for &value in vector {
        out.extend_from_slice(&f16::from_f32(value).to_bits().to_le_bytes());
    }
    out
}
```

见 [`src/quant/f16.rs:12-22`](../../src/quant/f16.rs)。四个 API 各管一段:

| 方法 | 方向 | 说明 |
|---|---|---|
| `f16::from_f32(x)` | `f32 → f16` | 就近舍入;`x` 超出可表示范围(舍入中点约 `±65520`)时饱和为 `±∞` |
| `f16::to_f32()` | `f16 → f32` | 无损宽化(`f16` 的每个值都能被 `f32` 精确表示) |
| `to_bits()` | `f16 → u16` | 取 16 位 IEEE 位模式,便于落盘 |
| `f16::from_bits(bits)` | `u16 → f16` | 从位模式还原,**不做浮点解释之外的处理** |

解码路径正好反过来:每 2 字节读成一个小端 `u16`,再 `from_bits().to_f32()`:

```rust
codes
    .chunks_exact(BYTES_PER_ELEMENT)
    .map(|pair| f16::from_bits(u16::from_le_bytes([pair[0], pair[1]])).to_f32())
    .collect()
```

见 [`src/quant/f16.rs:33-36`](../../src/quant/f16.rs)。行为上有三个必须记住的边界
(都有测试钉住,见 [`src/quant/f16.rs:105-116`](../../src/quant/f16.rs)):

- **相对精度约 `2^-11 ≈ 4.9e-4`**:f16 只有 10 位显式尾数,往返一次的最大相对误差就在这个量级;
  这也决定了它只用于**粗排**(先用量化副本选出候选),精排恒回退 f32 原向量(I12);
- **最大值 `±65504`**:`f16::from_f32` 是 IEEE 语义的"就近舍入",超出可表示范围的有限值
  (舍入中点约 `±65520`)会**饱和为 `±∞`**,而不是回绕成小数;查询侧的点积因此可能得到
  `inf`,但两阶段流程里粗排只负责排序;
- **`NaN`/`±Inf` 保持**:特殊值原样透传;`-0.0`、最小 subnormal、`±65504` 这些极值可以
  **逐位精确往返**。

> `to_bits`/`from_bits` 是"位模式"与"浮点值"之间的转换:`to_bits()` 得到 `u16` 而不是
> 某个"编码整数";反之 `from_bits` 是安全函数(不是 `unsafe`),因为它只重新解释 16 位,
> 任何 16 位模式都是合法的 f16(可能正好是 `NaN`)。
>
> 对比 §2.1 的 `as`:`as` 只能在标量数值类型之间转换,而标准库根本没有 `f16` 这个类型,
> 必须走 `half` crate 的显式 API——这也让"发生了舍入"在代码里一目了然。

### 2.4 布尔与字符

```rust
let is_enabled: bool = true;
let ch: char = '好';      // 单个 Unicode 标量值,占 4 字节
```

- 布尔变量按规范用 `is_` / `has_` / `can_` / `should_` 前缀。
- `char` 是**一个 Unicode 标量值**,不是字节;`char` 在内存里固定占 4 字节,
  但同一个字符在 UTF-8 字符串里只占 1–4 字节(`len_utf8()`);`"好"` 是字符串字面量
  (`&str`,见 [04 章](04-borrowing-strings-slices.md))。

`char` 还自带一组 Unicode 判定方法,L4 的 DSL 解析器用它划分"标识符 / 空白 / 数字":

```rust
let c: char = '好';
c.is_alphabetic();     // true:是字母(汉字也算,比"是不是 ASCII 字母"宽松)
c.is_alphanumeric();   // true:字母或数字
c.is_whitespace();     // 空格、制表符、换行……各类 Unicode 空白
c.len_utf8();          // 3:'好' 在 UTF-8 里占 3 个字节
```

- 解析器按**字节位置**推进(便于报"第 N 字节"错误),但读取时用 `chars().next()`
  拿下一个字符,再用 `len_utf8()` 换算回字节数;ASCII 数字与符号则直接看
  `as_bytes()` 里的 `u8`。
- 别把 `char` 当 `u8`:一个汉字 3 字节、一个 emoji 常是 4 字节,
  `self.pos += 1` 会切在字符中间,后续 `&input[pos..]` 直接 panic。

见 [`src/query/parse/parser.rs:76-85`](../../src/query/parse/parser.rs) 与
[`src/query/parse/grammar.rs:138-155`](../../src/query/parse/grammar.rs)。

### 2.5 数组与元组

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

mneme 的 `decode_u64` 返回 `Result<(u64, usize)>`,见 [`src/core/varint.rs:97`](../../src/core/varint.rs)。

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

见 [`src/core/types.rs:16`](../../src/core/types.rs)。因为 `u64` 是 `Copy`,所以 `RowId` 也是 `Copy`:
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
    pub(crate) store: Option<Arc<Store>>,                 // L2 持久层协调句柄;纯内存库为 None
    pub(crate) maintenance: Option<MaintenanceHandle>,    // L5 后台维护线程句柄
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

见 [`src/memory/table/state/`](../../src/memory/table/state/)。因此"给写状态拍快照"(`WriterState::clone`)
只是复制一批 `Arc` 句柄,非常廉价——这是写事务失败回滚与读者无锁扫描的共同前提
(用法见 [04 §5.1](04-borrowing-strings-slices.md) 与 [07 §4.3](07-iterators-closures.md))。

> 记录体里的向量也用 `Arc<[f32]>`(`SlotData::vector`):克隆一条记录只加计数,只有真正
> 替换向量时才分配新数组。L3 的 `IndexNode::vector` 同样是 `Arc<[f32]>`,构建索引节点时
> 把 `Vec<f32>` 转成"不可增长、共享只读"的 `Arc<[f32]>`:
>
> ```rust
> let vector: Vec<f32> = ...;
> let shared: Arc<[f32]> = Arc::from(vector.into_boxed_slice());
> ```
>
> 见 [`src/index/hnsw/build.rs:293-297`](../../src/index/hnsw/)。`into_boxed_slice()` 把
> `Vec<T>` 收缩成 `Box<[T]>`(丢掉多余容量,长度固定),`Arc::from` 再接管这块内存。
> 此后每次克隆都只是引用计数 +1,索引与段数据因此可以零拷贝共享同一份向量——和
> `Arc<str>` 是同一个套路,只是元素从 `u8` 换成了 `f32`。

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
| `attempt to ... with overflow` | debug 下整数溢出 | 确认输入范围,或用 `checked_*`/`saturating_*` |
| `the trait bound ... From<...> is not satisfied` | 转换不是"永远成功" | 用 `TryFrom`/`try_from` 显式处理失败 |

---

## 6. 本章小结

- `let` 默认不可变,要改加 `mut`;常量用 `const` + 全大写下划线。
- 整数按位宽/符号分很多种,Rust **不做隐式转换**;`as` 会静默截断,不允许丢数据时用
  `TryFrom`/`checked_*`/`saturating_*`;浮点比较用误差阈值,`is_finite`/`clamp` 防 NaN 与失控。
- 浮点工具还有 `round`(就近舍入)与 `powi`(整数次幂);L6 的 f16 副本用 `half::f16` 的
  `from_f32`/`to_f32`/`to_bits`/`from_bits`,相对精度约 `2^-11`、最大值 `±65504`、
  溢出饱和为 `±∞`、`NaN`/`±Inf` 保持。
- L4 用的整数方法:`div_euclid`/`rem_euclid`(负数 floor 除法)、`div_ceil`(向上取整)、
  `unsigned_abs`;`f64` 只精确到 2^53,整数转浮点比较前必须先检查范围。
- `char` 不是 `u8`:用 `chars().next()` 拿字符、`len_utf8()` 换算字节数,别在 UTF-8 中间切刀。
- 数组定长、元组可异构;需要可增长列表用 `Vec`。
- **所有权**:每个值一个所有者,离开作用域自动释放;赋值对堆类型是**移动**,对 `Copy` 类型是复制;
  想复制堆数据用 `.clone()`。
- **共享所有权**用 `Arc<T>`(`Arc::clone` 只加计数);要改共享数据用 `Arc::make_mut` 写时复制;
  `Arc<[f32]>` 可由 `Arc::from(vec.into_boxed_slice())` 构造,适合共享只读的大数组。
- newtype 用类型区分语义,把错误挡在编译期。

## 动手练习

1. 在 `examples/hello.rs` 里声明 `let x: u32 = 5;`,试着传给一个需要 `u64` 的函数,读报错并修正。
2. 写 `let s1 = String::from("a"); let s2 = s1;`,再打印 `s1`,观察移动报错;改成 `s1.clone()` 后通过。
3. 给 `examples/hello.rs` 加一个 `const MAX_ROWS: u32 = 1_000_000;` 并用 `println!` 打印。
4. 对 `-1_i64` 分别计算 `/ 86_400_000`、`div_euclid(86_400_000)`、`rem_euclid(86_400_000)`,
   解释三者为什么不同。
5. 读 [`src/quant/f16.rs:70-116`](../../src/quant/f16.rs) 的三个测试,再用 `half` crate 写一个
   `f32 → f16 → f32` 往返,对 `0.1`、`-12.345`、`1.0e6` 打印结果,观察相对精度、溢出饱和与
   特殊值行为。

## 下一章

[03 结构体、枚举与 impl](03-structs-enums-impl.md):用类型描述数据,用 `impl` 给类型挂行为。
