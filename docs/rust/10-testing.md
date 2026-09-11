# 10 测试与属性测试

> **本章目标**:会写单元测试、集成测试、doctest,理解 `proptest` 属性测试与 mneme 的契约追溯方式。
> **前置**:[05 章](05-errors.md)(`Result`)、[07 章](07-iterators-closures.md)。
> **对应源码**:各 `src/core/*.rs` 底部的 `mod tests`,以及 [`tests/core_contracts.rs`](../../tests/core_contracts.rs)、
> [`tests/contract_traceability.rs`](../../tests/contract_traceability.rs)、
> [`tests/l4_contracts.rs`](../../tests/l4_contracts.rs)、
> [`src/index/hnsw.rs`](../../src/index/hnsw.rs)、[`src/index/hidx.rs`](../../src/index/hidx.rs)、
> [`src/index/filtered.rs`](../../src/index/filtered.rs)。

mneme 把测试当作**形式化约束的证明**:每个公开行为都要有测试,每个测试要能追溯到一条契约
(`FC-*`)。本项目的开发流程是"先写契约 → 再写测试 → 再写实现"(FSVDD,见
[CONTRIBUTING.md](../../CONTRIBUTING.md))。

---

## 1. 单元测试:和被测代码放一起

Rust 惯例:在源文件底部加一个测试模块。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newtype_roundtrips_raw_value() {
        assert_eq!(RowId::new(7).get(), 7);
        assert_eq!(SlotId::new(9).get(), 9);
        ...
    }
}
```

见 [`src/core/types.rs:267-278`](../../src/core/types.rs)。(`...` 处源码里还有
`SeqNo` / `SegmentId` / `NsId` 三个断言,此处节选。)

- `#[cfg(test)]` 让这个模块**只在 `cargo test` 时编译**,不会进发布产物。
- `use super::*;` 把父模块的所有东西引入测试作用域。
- `#[test]` 标记一个测试函数;函数**无参数**。返回值可以是 `()`(失败靠 panic),也可以是
  `Result<(), E>`(`Err` 即测试失败,便于在测试里用 `?`)。
- 测试名用**行为描述**,不用 `test1`;如 `rejects_empty_user_id`。

### 1.1 断言宏

```rust
assert!(x > 0);                       // 条件为真
assert_eq!(a, b);                     // 相等
assert_ne!(a, b);                     // 不等
assert_eq!(a, b, "自定义失败信息 {a}"); // 带格式化信息
```

- 断言具体值,避免只写 `assert!(result.is_ok())` 这种**弱断言**。
- 浮点用误差:`assert!((s - 0.8).abs() < 1e-6);`,见 [`src/core/metric.rs:135`](../../src/core/metric.rs)。

### 1.2 AAA 结构

一个测试通常分三段:

```rust
#[test]
fn expired_session_is_invalid() {
    // Arrange(准备)
    let session = create_session(input, 0);
    // Act(执行)
    let invalid = is_invalid(&session, 1_000);
    // Assert(断言)
    assert!(invalid);
}
```

### 1.3 测试里的 `unwrap`

生产代码禁止 `unwrap`,但**测试里允许**——测试失败就该炸,且 `unwrap` 的报错信息够用。
mneme 的集成测试用 `.expect("下界合法")` 让失败信息更清楚,见
[`tests/core_contracts.rs:45`](../../tests/core_contracts.rs)。

### 1.4 测试替身:可注入的假时钟

验证 TTL、`as_of` 历史视图这类行为的测试**不能读真实时间**,否则结果不可重现。L4 的契约测试
把时间做成可手动推进的 `Clock` 实现:

```rust
struct FakeClock(AtomicI64);

impl Clock for FakeClock {
    fn now_unix_ms(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}
```

见 [`tests/l4_contracts.rs:31-38`](../../tests/l4_contracts.rs)。要点:

- `Clock` 是 L0 定义的 trait(见 [04 §5](04-borrowing-strings-slices.md));测试实现它,
  再用 `.clock(Arc::clone(&clock) as Arc<dyn Clock>)` 注入构建器,业务代码读到的"现在"
  就由测试说了算(见 [`tests/l4_contracts.rs:539-542`](../../tests/l4_contracts.rs),
  推进时间用 `clock.0.store(...)`,即 `AtomicI64` 的 `&self` 写入)。
- 内部用 `AtomicI64` 而不是 `Cell`:`&self` 下也能改,且跨线程安全(见
  [04 §5.2](04-borrowing-strings-slices.md))。
- 这是"外部状态必须通过参数注入"(见 [CONTRIBUTING.md](../../CONTRIBUTING.md))的落地;
  随机数、环境变量同理,不要直接读全局。

> 仓库里还有一份共享的 `FakeClock(Arc<Mutex<i64>>)`,放在 [`tests/common/mod.rs`](../../tests/common/mod.rs),
> 供其余契约测试复用;两种写法都行,关键是"测试能控制时间"。

---

## 2. 集成测试:`tests/` 目录

`tests/` 下每个 `.rs` 文件是一个**独立的 crate**,只能通过**公开 API**访问库:

```rust
use mneme::{Dimension, Metric, TopK, json};
use mneme::simd::{dot, dot_scalar};
```

见 [`tests/core_contracts.rs:20-24`](../../tests/core_contracts.rs)。

- 集成测试验证"用户视角"的 API 是否按契约工作。
- 单元测试可以访问私有项,集成测试不行——这个区别很有用:它强迫你验证公开契约。

---

## 3. doctest:文档里的测试

如 [08 章](08-modules-docs.md) 所述,`///` 文档里的 ```` ``` ```` 代码块会被 `cargo test` 执行:

````rust
/// # Examples
///
/// ```
/// use mneme::Metric;
/// assert_eq!(Metric::Dot.score(&[1.0, 2.0], &[3.0, 4.0], 0.0, 0.0), 11.0);
/// ```
````

好处:**示例永远可运行**,文档不会腐烂。mneme 每个非平凡公开 API 都有 doctest。

---

## 4. 属性测试(property-based testing):用 `proptest`

手写测试只能覆盖你想到的输入。**属性测试**自动生成成百上千组随机输入,验证**不变量**。

mneme 用 `proptest` crate(在 `[dev-dependencies]` 里):

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn varint_roundtrip_minimal(value in any::<u64>()) {
        let mut buf = Vec::new();
        encode_u64(value, &mut buf);
        prop_assert_eq!(decode_u64(&buf).unwrap(), (value, buf.len()));
        ...
    }
}
```

见 [`tests/core_contracts.rs:186-202`](../../tests/core_contracts.rs)。(`...` 处源码里还断言了
最小编码字节数,并对 `u32` 做同样的往返检查。)

- `value in any::<u64>()` 是**策略(strategy)**:告诉 proptest 生成任意 `u64`。
- `prop_assert_eq!` 是 proptest 版断言,失败时会**收缩(shrink)**到最小反例。
- 区间生成:`prop::collection::vec(-10.0f32..10.0, 0..300)` 生成"长度 0–300、元素在 -10~10 的 Vec"。
- 失败时 proptest 会打印反例,你可以据此加一个固定的回归测试。

### 4.1 失败之后:收缩与回归

proptest 发现反例后会**收缩(shrink)**到"最小"的失败输入,并把它打印出来。你可以把这个反例
原样抄成一个普通 `#[test]` 作为**回归测试**,确保以后不再犯。proptest 默认还会把失败用例写进
`proptest-regressions/` 目录(应提交进 git),下次直接重放。

> `prop_assert!` / `prop_assert_eq!` 与普通 `assert!` 的区别就在于:前者让 proptest 能捕获失败并
> 继续收缩,后者会直接 panic 打断收缩过程。

### 4.2 属性 vs 例子

| | 例子测试(example-based) | 属性测试(property-based) |
|---|---|---|
| 输入 | 手写几个 | 自动生成大量随机 |
| 验证 | 具体输出 | 通用不变量 |
| 优点 | 直观、快 | 覆盖想不到的边界 |
| 缺点 | 覆盖有限 | 需想清楚"不变量是什么" |

### 4.3 参照实现:验证"优化版"与"朴素版"一致

mneme 的 `dot` 有 SIMD 优化版和标量参照版。属性测试断言两者一致:

```rust
proptest! {
    #[test]
    fn dot_matches_scalar_reference(
        a in prop::collection::vec(-10.0f32..10.0, 0..300),
        b in prop::collection::vec(-10.0f32..10.0, 0..300),
    ) {
        let len = a.len().min(b.len());
        let a = &a[..len];
        let b = &b[..len];
        let vectorized = dot(a, b);
        let scalar = dot_scalar(a, b);
        let tolerance = 1e-4 * (1.0 + scalar.abs());
        prop_assert!(
            (vectorized - scalar).abs() <= tolerance,
            "dot={vectorized} scalar={scalar}"
        );
    }
}
```

见 [`tests/core_contracts.rs:204-220`](../../tests/core_contracts.rs)。这是优化代码最有力的正确性保障。

### 4.4 参照实现:验证数据结构

`TopK` 是堆实现;测试用一个"排序后取前 k"的朴素参照实现来对照:

```rust
fn reference_topk(entries: &[(f32, u32)], k: usize, metric: Metric) -> Vec<u32> {
    let mut ordered = entries.to_vec();
    ordered.sort_by(|a, b| {
        if metric.better(a.0, b.0) {
            Ordering::Less
        } else if metric.better(b.0, a.0) {
            Ordering::Greater
        } else {
            a.1.cmp(&b.1)          // 同分按载荷升序,与 TopK 的 tie-break 对齐
        }
    });
    ordered.truncate(k);
    ordered.into_iter().map(|(_, id)| id).collect()
}
```

见 [`tests/core_contracts.rs:26-40`](../../tests/core_contracts.rs) 与 [`tests/core_contracts.rs:156-184`](../../tests/core_contracts.rs)。

### 4.5 自定义策略:`impl Strategy` 与 `prop_oneof!`

上面的策略都是现成的(`any::<u64>()`、`prop::collection::vec(...)`)。L3 的 hidx 测试需要
"合法的二进制文件"和"在合法文件上翻一个字节的变异体"作为输入,于是把策略写成了**函数**:

```rust
/// 合法 hidx 编码(随机层级、合法邻居、入口取最高层节点),作为变异基底。
fn valid_hidx_strategy() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(0u8..=2, 1..=8).prop_map(|levels| {
        let mut graph = Graph::new();
        ...
        encode(&graph, GRAPH_PARAMS).expect("合法图必须可编码")
    })
}

/// 在合法编码上翻一个字节并重算对应 CRC。
fn mutated_valid_hidx_strategy() -> impl Strategy<Value = Vec<u8>> {
    (valid_hidx_strategy(), any::<usize>(), any::<u8>()).prop_map(|(mut bytes, pos, value)| {
        let index = pos % bytes.len();
        bytes[index] = value;
        ...
        bytes
    })
}
```

见 [`src/index/hidx.rs:727-767`](../../src/index/hidx.rs)。三个新工具:

- **`impl Strategy<Value = T>`**:策略也是值,可以写成函数返回。`impl Trait` 返回类型让调用者
  不必关心具体策略类型(见 [06 §2.3](06-generics-traits.md));
- **`prop_map`**:把生成器的输出映射成另一个值——这里是"随机层级序列 → 建图 → 编码成字节",
  相当于迭代器的 `map`,描述的是"怎么造输入";
- **元组的 `Strategy`**:`(a, b, c)` 本身也是一个策略,依次生成三个值再合并——`mutated_...`
  用它同时取"合法文件 + 位置 + 新字节"。

再到使用处,用 `prop_oneof!` 把多来源策略混在一条测试里:

```rust
proptest! {
    #[test]
    fn hidx_decode_never_panics_on_arbitrary_bytes(
        bytes in prop_oneof![
            proptest::collection::vec(any::<u8>(), 0..4096),   // ① 任意字节
            valid_hidx_strategy(),                             // ② 合法文件
            mutated_valid_hidx_strategy(),                     // ③ 合法文件变体
        ]
    ) {
        let Ok(decoded) = decode(&bytes) else {
            return Ok(());
        };
        // 解码成功 → 必须能重编码并往返一致
        ...
    }
}
```

见 [`src/index/hidx.rs:769-801`](../../src/index/hidx.rs)。要点:

- `prop_oneof![a, b, c]` 每次从几个策略中随机挑一个执行,适合"混合来源"的输入;
- `return Ok(())` 在 `proptest!` 宏里表示"本用例通过"——测试体返回
  `Result<(), TestCaseError>`,所以 `let...else`(见 [05 §4.6](05-errors.md))失败时直接
  判该用例通过:任意字节被**拒绝**也是合法结局,"不 panic + 接受即往返"才是要验证的属性;
- 纯随机字节几乎不可能穿过魔数与 CRC 校验,所以必须补"合法样本"与"变异样本"两类策略,
  才能逼着解析器走到布局、入口、邻居等每个语义校验分支。

> 这种"合法样本 + 在其上做变异"的策略就是 **fuzzing 思路**:变异体专门冲撞解析器的
> 校验边界。对应 [CONTRIBUTING.md](../../CONTRIBUTING.md) 的证伪原则——故意破坏一条约束,
> 必须有测试变红。hidx 的变异策略还会**重算 CRC**,否则变异体会先被 CRC 拦下,
> 根本到不了真正要测的语义校验。

### 4.6 操作计数探针:用 `thread_local!` 验证复杂度

mneme 的契约不只约束结果,还约束**代价**(`FC-*-CPLX-*`)。"构建距离计算随节点数近似线性"
怎么测?L3 在距离函数里埋了一个**线程局部计数器**:

```rust
// 单测操作计数:统计距离计算次数(线程局部,避免测试间干扰)。
#[cfg(test)]
thread_local! {
    static DIST_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn bump_dist_calls() {
    DIST_CALLS.with(|calls| calls.set(calls.get() + 1));
}
```

见 [`src/index/hnsw.rs:36-45`](../../src/index/hnsw.rs)。逐个概念:

- **`thread_local!`**:声明"每个线程各有一份"的静态变量。测试默认并行、各跑各的线程,
  全局计数器会互相污染;线程局部正好把每个测试隔开;
- **`Cell<u64>`**:最简单的**内部可变性**容器(见 [04 §5](04-borrowing-strings-slices.md)):
  没有 `&mut` 也能改,`get()` 复制出值、`set()` 覆盖。单线程够用、没有原子开销;
- **`const { Cell::new(0) }`**:`const` 块在编译期求值,给线程局部变量提供常量初始化
  (`Cell::new` 是 `const fn`,所以合法),比惰性初始化更快、语义更简单;
- `DIST_CALLS.with(|calls| ...)`:访问当前线程的那份变量。`.with` 是必须的——它保证
  你拿到的是本线程的值,而不是某个跨线程共享的静态量。

测试再用计数验证**增长率**:

```rust
let c200 = build_calls(200);
let c500 = build_calls(500);
let c1200 = build_calls(1200);
let r1 = c500 as f64 / c200 as f64;
let r2 = c1200 as f64 / c500 as f64;
assert!(r1 < 4.0, "200→500 增长过快(疑似二次):{r1}");
assert!(r2 < 4.0, "500→1200 增长过快(疑似二次):{r2}");
```

见 [`src/index/hnsw.rs:534-549`](../../src/index/hnsw.rs)。要点:

- 断言的是**规模之间的比值**,不是绝对次数——常数随实现微调而变,但"线性时 2.5 倍节点
  对应约 2.5 倍操作数,二次时约 6.25 倍"这个结构性质稳定;
- 这是"复杂度上界测试"的落地方式:不看秒表(受机器负载影响大),而是数**基本操作**;
- 探针代码全部在 `#[cfg(test)]` 下,发布产物里一行不剩。测试里先重置计数
  (`DIST_CALLS.with(|calls| calls.set(0))`)再执行,读取时把 `Cell::get` 当函数指针传给
  `with`(`DIST_CALLS.with(std::cell::Cell::get)`),见
  [`src/index/hnsw.rs:496-500`](../../src/index/hnsw.rs)。

> 同类探针在 `filtered.rs` 里记录"最近一次搜索命中的档位与 `ef`",用于把档位分派公式
> 逐值钉死;`thread_local!` 保证并行测试互不干扰(见
> [`src/index/filtered.rs:37-63`](../../src/index/filtered.rs))。

---

## 5. 契约追溯:测试不是"测了个寂寞"

mneme 的集成测试文件头部列出它覆盖的契约编号:

```rust
//! * FC-CORE-PRE-001  —— 维度闭区间 `[1, 65536]`
//! * FC-CORE-POST-001 —— `Metric::score` 三分支数学映射
//! * FC-CORE-INV-001  —— `simd::dot` ≡ 标量参考
//! ...
```

见 [`tests/core_contracts.rs:3-16`](../../tests/core_contracts.rs),契约定义在
[`docs/spec/contracts.md`](../spec/contracts.md)。

- 每个测试函数上方注释它对应的 `FC-*` 编号。
- 设计约束 → 契约编号 → 测试用例,形成**1:1 追溯矩阵**。
- 新增业务逻辑必须登记至少一条契约并给出测试;CI 校验 100% 追溯。

> 这就是 FSVDD(形式化规范与验证驱动开发)在 mneme 的落地方式。

### 5.1 门禁:`include_str!` 元测试

追溯光靠自觉会漂移,所以 mneme 用一段**元测试**机械校验。它在编译期把契约与测试源码
整个嵌进测试二进制:

```rust
const CONTRACTS: &str = include_str!("../docs/spec/contracts.md");
const MEMORY_TESTS: &str = include_str!("memory_contracts.rs");
```

见 [`tests/contract_traceability.rs`](../../tests/contract_traceability.rs)。然后在测试里做字符串解析,
校验四件事:

- 契约引用的每个 `文件.rs::测试名` 必须真实存在(路径级匹配,无悬空引用);
- 测试文件里每个 `#[test]` 必须被至少一条契约引用(无孤立测试);
- 状态为 `Passed` 的契约必须登记真实测试(禁止"声称通过却没有证据");
- 测试文件头声明的 `FC-*` 集合与契约覆盖该文件的条目**双向相等**(无多报、无漏报)。

L1 的契约测试按 FC 模块族拆分为 `tests/memory_contracts.rs`、`query_contracts.rs`、
`model_contracts.rs`、`life_contracts.rs`;L2/L3/L4 的 `persist_contracts.rs`、
`hnsw_contracts.rs`、`l4_contracts.rs` 也一并纳入这道门禁。它随 `cargo test` 一起跑——
契约漂移会让测试变红,而不是等人工审查发现。

> `include_str!` 是"把别的文件当字符串嵌入当前源码"的编译期宏;配合 `env!("CARGO_PKG_VERSION")`
> 之类的 `include_*!` 家族,常用来做配置/模板/契约的机械校验。

---

## 6. 运行测试

```bash
cargo test                          # 全部(单测 + 集成 + doctest)
cargo test --lib                    # 只跑库内单元测试
cargo test --test core_contracts    # 只跑某个集成测试文件
cargo test varint                   # 只跑名字含 "varint" 的测试
cargo test -- --nocapture           # 显示 println! 输出
cargo test --release                # release 模式跑(测优化后的行为)
```

> 注意:release 与 debug 行为可能不同(如整数溢出、`debug_assert!`)。
> 涉及这些的代码应在两种模式下都测。
>
> 测试默认**并行**运行,所以不要依赖执行顺序或共享全局状态(这也是 mneme 用可注入时钟的原因)。
> 需要串行时用 `cargo test -- --test-threads=1`。

---

## 7. 你会遇到的问题

| 现象 | 原因 | 修法 |
|---|---|---|
| 测试没被收集 | 函数缺 `#[test]` 或模块缺 `#[cfg(test)]` | 补属性 |
| `#[test]` 函数带参数 | 测试函数不能带参数 | 去掉参数;需要输入就写死在测试里 |
| `#[test]` 返回非 `()`/`Result` | 返回类型不被支持 | 返回 `()` 或 `Result<(), E>` |
| 测试顺序依赖 | 用了全局状态/文件 | 每个测试独立,用注入的假时钟 |
| proptest 找不到反例 | 策略区间太窄或断言错误 | 放宽策略、加日志 |
| doctest 失败 | 文档示例与实现不符 | 修示例或修实现(见 [08 章](08-modules-docs.md)) |

---

## 8. 本章小结

- 单元测试放 `#[cfg(test)] mod tests`,用 `#[test]` 和断言宏;遵循 AAA,测试名描述行为。
- 集成测试放 `tests/`,只能访问公开 API;doctest 让文档示例自动运行。
- 时间等外部状态用**测试替身**注入(如实现 `Clock` 的 `FakeClock`),测试才能重现。
- `proptest` 自动生成随机输入验证不变量,失败会收缩到最小反例;常用"参照实现"对照优化实现。
- 策略可以组合:自定义 `impl Strategy` 函数、`prop_map` 变换、`prop_oneof!` 混合来源;
  "合法样本 + 变异"的 fuzzing 思路能逼解析器走遍校验分支。
- 复杂度也是可测的:测试专属的 `thread_local!` + `Cell` 探针统计基本操作次数,
  断言规模之间的增长率而非绝对耗时。
- mneme 用 `FC-*` 契约编号把约束、实现、测试串成 1:1 追溯矩阵(FSVDD),并由
  `tests/contract_traceability.rs` 机械校验(含 `include_str!` 元测试)。
- `cargo test` 一次跑齐单测、集成、doctest。

## 动手练习

1. 给 [03 章练习](03-structs-enums-impl.md)的 `Shape::area` 写单元测试,覆盖圆、矩形、边界值(0、负数)。
2. 把 `Shape::area` 的测试改成 `proptest`:任意正数半径/边长的面积都 ≥ 0。
3. 运行 `cargo test`,找到 `tests/core_contracts.rs` 里一个测试对应的 `FC-*` 编号,在
   [`docs/spec/contracts.md`](../spec/contracts.md) 中读它的完整定义。

## 结语

到这里,你已经掌握了读懂 mneme L0 所需的全部 Rust 基础。L1( [`src/memory/`](../../src/memory) )
新引入的共享所有权、锁与写事务等特性,L3( [`src/index/`](../../src/index) )
新引入的手写 `Ord`、`BinaryHeap`、`TryFrom`/受检运算、字节切片操作、let 链与 `let...else`、
`thread_local!` 探针、proptest 自定义策略,以及 L4( [`src/query/`](../../src/query) )
新引入的递归枚举与 `Box`、生命周期参数化解析器、`HashMap` entry API、原子类型、
运算符重载与 `fmt::Write`、可注入假时钟等,都已回填到 01–08 与 10 章的对应小节
(回填总表见 [README §4](README.md))。建议现在从头再读一遍
[`src/core/`](../../src/core) 的源码,把每处语法对应回相应章节;有余力再按需读
[`src/memory/`](../../src/memory)、[`src/index/`](../../src/index) 与
[`src/query/`](../../src/query)。之后可以按
[DESIGN.md](../DESIGN.md) 的分层路线,从 [03 L1 内存引擎](../design/03-l1-memory.md)
起继续读各层设计文档,并参考 [README 的通用资料](README.md#5-学完之后的下一步通用资料)继续深入 Rust。
