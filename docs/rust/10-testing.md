# 10 测试与属性测试

> **本章目标**:会写单元测试、集成测试、doctest,理解 `proptest` 属性测试与 mneme 的契约追溯方式。
> **前置**:[05 章](05-errors.md)(`Result`)、[07 章](07-iterators-closures.md)。
> **对应源码**:各 `src/core/*.rs` 底部的 `mod tests`,以及 [`tests/core_contracts.rs`](../../tests/core_contracts.rs)。

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
- `proptest` 自动生成随机输入验证不变量,失败会收缩到最小反例;常用"参照实现"对照优化实现。
- mneme 用 `FC-*` 契约编号把约束、实现、测试串成 1:1 追溯矩阵(FSVDD)。
- `cargo test` 一次跑齐单测、集成、doctest。

## 动手练习

1. 给 [03 章练习](03-structs-enums-impl.md)的 `Shape::area` 写单元测试,覆盖圆、矩形、边界值(0、负数)。
2. 把 `Shape::area` 的测试改成 `proptest`:任意正数半径/边长的面积都 ≥ 0。
3. 运行 `cargo test`,找到 `tests/core_contracts.rs` 里一个测试对应的 `FC-*` 编号,在
   [`docs/spec/contracts.md`](../spec/contracts.md) 中读它的完整定义。

## 结语

到这里,你已经掌握了读懂 mneme L0 所需的全部 Rust 基础。建议现在从头再读一遍
[`src/core/`](../../src/core) 的源码,把每处语法对应回相应章节。之后可以按
[DESIGN.md](../DESIGN.md) 的分层路线继续读 L1–L6 的设计文档,并参考 [README 的通用资料](README.md#5-学完之后的下一步通用资料)继续深入 Rust。
