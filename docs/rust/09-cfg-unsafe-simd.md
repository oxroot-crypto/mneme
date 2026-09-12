# 09 条件编译、unsafe 与 SIMD

> **本章目标**:理解 `#[cfg(...)]` 条件编译、`unsafe` 的含义与边界,以及 mneme 如何用 SIMD
> 内联指令加速点积。
> **前置**:[04 章](04-borrowing-strings-slices.md)(切片与引用)、[08 章](08-modules-docs.md)。
> **对应源码**:[`src/core/simd.rs`](../../src/core/simd.rs)。

这一章涉及 Rust 里唯一"绕过编译器保护"的部分。**mneme 把 `unsafe` 压缩到全库仅两处**
(L0 `src/core/simd.rs` 的 arch 内联与 L2 `src/persist/source.rs` 的 `MmapSource`),
每处都附 `// SAFETY:` 证明,是学习"如何负责任地使用 unsafe"的范本。

---

## 1. 条件编译:`#[cfg(...)]`

同一份源码要编译到不同平台时,用**条件编译**选择性地包含代码。

```rust
// 编译期:整段函数/模块按平台保留或删除
#[cfg(target_arch = "x86_64")]
fn dot_x86(a: &[f32], b: &[f32]) -> f32 { ... }

#[cfg(target_arch = "aarch64")]
mod neon { ... }

// 同一函数体内,用 #[cfg] 选中某一条语句/块表达式
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    #[cfg(target_arch = "x86_64")]
    { dot_x86(a, b) }

    #[cfg(target_arch = "aarch64")]
    { unsafe { neon::dot_neon(a, b) } }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    { dot_scalar(a, b) }
}
```

见 [`src/core/simd.rs:50-65`](../../src/core/simd.rs)。

> **`#[cfg]` 不只用在函数/模块上**,也能用在 `struct` 字段、`match` 分支,以及函数体里的
> **语句/块表达式**上。上面 `dot` 就是后者:三个互斥的块各自带 `#[cfg]`,编译后只留下一个。
> 注意块本身仍要写成 `{ ... }`,属性写在块前面。注意:上例为教学而把
`simd.rs` 不同位置的片段拼在一起——`dot` 内部三段 `#[cfg]` 分发(含
`not(any(...))` 兜底)才是 50–65 行的连续原文;`fn dot_x86` 在 100 行附近,
`mod neon` 在 182 行附近。

常用条件:

| 条件 | 含义 |
|---|---|
| `target_arch = "x86_64"` | CPU 架构 |
| `target_os = "windows"` | 操作系统 |
| `feature = "async"` | Cargo feature 开启 |
| `test` | 正在编译测试(`#[cfg(test)]`) |
| `debug_assertions` | debug 构建 |
| `not(...)` / `any(...)` / `all(...)` | 逻辑组合 |

> `feature = "..."` 只在 `Cargo.toml` 声明了对应 feature 时才有意义。mneme 目前声明了
> `mmap`(默认开启,见 [01 §4.2](01-toolchain.md))、`quant-f16`、`async` 与 `fuzzing`;
> `encrypt`/`compress`/`compress-zstd`/`wasm` 等尚未定义,上表仅作语法示例。

### 1.1 `cfg!` 宏:运行期用的布尔值

```rust
if cfg!(target_arch = "x86_64") { ... }
```

区别:`#[cfg]` 在**编译期把代码整段删掉**;`cfg!` 在编译期展开成常量 `true`/`false`,**两个分支都仍会被编译和类型检查**,只是死分支会被优化掉。所以需要"两个分支都得能编译"时用 `cfg!`,需要"平台相关代码根本不参与编译"时用 `#[cfg]`。

### 1.2 mneme 的运行时分发

`dot` 先做编译期架构选择,再在 x86_64 上做**运行时 CPU 特性检测**:

```rust
if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
    unsafe { x86::dot_avx2(a, b) }
} else {
    unsafe { x86::dot_sse2(a, b) }
}
```

见 [`src/core/simd.rs:100-108`](../../src/core/simd.rs)。

- `is_x86_feature_detected!` 是标准库宏,在**运行时**查询 CPU 是否支持某指令集。
- 这样同一个二进制能跑在支持 AVX2 的新 CPU(快)和不支持的旧 CPU(回退 SSE2)上。

---

## 2. `unsafe`:什么时候必须用

Rust 的安全性由编译器保证,但有些底层操作编译器无法验证,必须由程序员**承诺**自己保证正确。
这些操作包括:

- 解引用**裸指针(raw pointer)** `*const T` / `*mut T`;
- 调用 `unsafe fn`(如 SIMD 内联函数);
- 访问 `union` 字段;
- 修改可变静态变量;
- 实现 `unsafe trait`。

`unsafe` **不是关闭所有检查**:借用规则、类型检查仍然生效;它只是让你能执行上述操作。

### 2.1 `unsafe` 块 vs `unsafe fn`

```rust
// 调用者必须保证前置条件
pub unsafe fn dot_avx2(a: &[f32], b: &[f32]) -> f32 {
    // 在函数内部,不安全操作还要再包一层 unsafe 块
    let mut sum = unsafe { ... };
    ...
}
```

见 [`src/core/simd.rs:124-128`](../../src/core/simd.rs)。

- `unsafe fn` 表示"调用此函数需要满足某些前提"。
- `unsafe { ... }` 块表示"这里我做了不安全操作,并为此负责"。
- mneme 开启 `#![deny(unsafe_op_in_unsafe_fn)]`:在 `unsafe fn` 里也必须显式写 `unsafe` 块,
  避免"整个函数都是不安全的、没人知道危险在哪"。

### 2.2 `// SAFETY:` 证明(强制)

mneme 规范要求**每个 `unsafe` 块必须有 `// SAFETY:` 注释**,逐条说明为什么安全:

```rust
// SAFETY: 循环条件 i + 8 <= n 保证 [i, i+8) 落在两个切片范围内;CPU 支持已由调用者确认。
let mut sum = unsafe { ... };
```

见 [`src/core/simd.rs:127-128`](../../src/core/simd.rs)。

调用处也要证明:

```rust
// SAFETY: 已确认 avx2 与 fma 均可用;长度前提由 dot 的 debug 断言与 n 收敛保证。
unsafe { x86::dot_avx2(a, b) }
```

见 [`src/core/simd.rs:102-103`](../../src/core/simd.rs)。

> 没有 `// SAFETY:` 的 `unsafe` 一律视为违规。这不是形式主义:它把"为什么这段代码是安全的"
> 写进代码,让后来者能审计。

---

## 3. `#[target_feature]`:告诉编译器启用某指令集

```rust
#[target_feature(enable = "avx2", enable = "fma")]
pub unsafe fn dot_avx2(a: &[f32], b: &[f32]) -> f32 { ... }
```

见 [`src/core/simd.rs:123-124`](../../src/core/simd.rs)。

- 它让编译器为这个函数生成使用 AVX2/FMA 指令的代码。
- 因为目标 CPU 不一定支持,函数被标记为 `unsafe`,调用者必须先检测(§1.2)。

---

## 4. 裸指针与 `as_ptr().add(i)`

SIMD 内联函数需要指针:

```rust
let va = _mm256_loadu_ps(a.as_ptr().add(i));
```

见 [`src/core/simd.rs:131`](../../src/core/simd.rs)。

- `a.as_ptr()` 得到 `*const f32` 裸指针。
- `.add(i)` 指针算术,向后移动 `i` 个元素(不是字节)。
- `_mm256_loadu_ps` 从该地址读 8 个 `f32`(`u` 表示 unaligned,不要求对齐)。
- **安全性由循环条件 `i + 8 <= n` 保证**读写都在切片范围内,这就是 `// SAFETY:` 要说明的。

### 4.1 为什么用 `loadu`(unaligned)而不是 `load`

SIMD 加载分两种:`_mm256_loadu_ps`(u = unaligned)不要求地址按 32 字节对齐;`_mm256_load_ps`
要求对齐,否则是未定义行为。mneme 的切片来自 `Vec`/数组,不保证 SIMD 对齐,所以用 `loadu`,
代价通常可忽略。若以后要压榨性能,可对齐分配 + 用 `load`,但必须把对齐作为前置条件写进
`// SAFETY:`。

`.add(i)` 是**按元素**移动指针(不是按字节):`a.as_ptr().add(i)` 指向第 `i` 个 `f32`。指针算术
越界即使不解引用也是 UB,所以循环条件 `i + 8 <= n` 是安全证明的核心。裸指针不携带生命周期,
`unsafe` 块里的正确性完全由程序员用 `// SAFETY:` 论证——这正是 mneme 把 `unsafe` 压缩到
全库仅两处(L0 `simd.rs` 与 L2 `persist/source.rs` 的 `MmapSource`)、且每处必写证明的原因。

### 4.2 第二处 `unsafe`:L2 的 mmap

L2 用 `memmap2` 把段文件映射成内存,`Mmap::map` 本身是 `unsafe fn`:

```rust
// SAFETY: 段文件是 write-once 的——库内任何路径都不原地写入/截断已提交段
// (单写者经临时文件 + rename 生成新段);`Mmap::map` 要求映射期间文件不被
// 截断/改写,该不变量由存储层保证。映射长度取映射时刻的文件长度。
let map = unsafe { memmap2::Mmap::map(&file)? };
```

见 [`src/persist/source.rs:100-107`](../../src/persist/source.rs)。为什么必须 `unsafe`:
mmap 把文件"变成"一段内存,但**文件可能被其他进程截断或改写**,这段内存就可能失效
——Rust 的类型系统无法校验这一点,所以由调用者承担"映射期间文件不被修改"的责任并写进
`// SAFETY:`。mneme 的段文件是 write-once(内容不可变),这个前提成立。

**feature 双后端**也在这里:同一个类型名按 feature 编译成两种实现,互斥存在:

```rust
#[cfg(feature = "mmap")]
pub(crate) struct MmapSource { map: memmap2::Mmap }

// 关闭 feature 时的兜底后端(总是编译;mmap 开启时仅测试使用)
pub(crate) struct FileSource { file: Mutex<File> }
```

见 [`src/persist/source.rs:36-39`](../../src/persist/source.rs) 与
[`src/persist/source.rs:89-92`](../../src/persist/source.rs)。`#[cfg]` 不止能加在
`use`/函数上,还能加在**整个类型**、方法甚至语句/块上;`#[cfg_attr(feature = "mmap", allow(dead_code))]`
则是"条件满足时才附加属性"(见 [01 §4.2](01-toolchain.md))。

---

## 5. SIMD 是什么

**SIMD(Single Instruction Multiple Data,单指令多数据)**:一条指令同时处理多个数据。
普通标量循环一次乘加一个分量;AVX2 一次乘加 8 个 `f32`。这正是 [00 章](../design/00-fundamentals.md)
说"SIMD 可把点积从 $O(d)$ 降到约 $O(d/8)$ 次向量指令"的原因。

mneme 的策略:

1. 对外只暴露一个可移植的 `dot(a, b)`;
2. 内部按架构分发到 AVX2 / SSE2 / NEON / 标量;
3. `dot_scalar` 作为**参照实现**,测试用它验证 SIMD 结果(见 [10 章](10-testing.md))。

```rust
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "点积要求两向量等长");
    #[cfg(target_arch = "x86_64")] { dot_x86(a, b) }
    #[cfg(target_arch = "aarch64")] { unsafe { neon::dot_neon(a, b) } }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))] { dot_scalar(a, b) }
}
```

见 [`src/core/simd.rs:50-65`](../../src/core/simd.rs)。

- `debug_assert_eq!` 只在 debug 构建生效;release 下不检查(性能考虑)。
- 于是 SIMD 内核自己用 `n = a.len().min(b.len())` 收敛到较短长度,保证不越界。

---

## 6. 你会遇到的编译器报错

| 报错关键词 | 原因 | 修法 |
|---|---|---|
| `call to unsafe function is unsafe` | 调用 `unsafe fn` 没包 `unsafe {}` | 加 `unsafe` 块并写 `// SAFETY:` |
| `dereference of raw pointer is unsafe` | 解引用裸指针没包 | 同上 |
| `use of unstable ...` | 用了 nightly 特性 | 换稳定写法 |
| `#[target_feature]` + safe | 带 target_feature 的函数默认不安全 | 标 `unsafe fn` |
| `undefined reference to _mm...` | 缺少目标架构 | 用 `#[cfg(target_arch)]` 包起来 |

---

## 7. 本章小结

- `#[cfg(...)]` 编译期选择平台相关代码,`cfg!` 运行期判断;`is_x86_feature_detected!` 运行时探测 CPU。
- `unsafe` 只解锁底层操作,不关闭借用/类型检查;每处必须写 `// SAFETY:` 证明。
- `#[target_feature]` 为函数启用指令集,因此函数必须 `unsafe` 且调用前检测。
- SIMD 用一条指令处理多个数据;mneme 对外统一 `dot`,内部按架构分发,`dot_scalar` 作参照。
- mneme 把 `unsafe` 限制在全库两处(`simd.rs` 与 `persist/source.rs` 的 `MmapSource`),
  配合 `#![deny(unsafe_op_in_unsafe_fn)]` 强制显式。

## 动手练习

1. 写一个 `fn is_big_endian() -> bool { cfg!(target_endian = "big") }`,在两个平台上都编译通过。
2. 阅读 [`src/core/simd.rs`](../../src/core/simd.rs) 的 `dot_sse2`,找出循环条件如何保证不越界,
   并在纸上写出对应的 `// SAFETY:` 论证。
3. 运行 `cargo test`,观察 `dot_matches_scalar_reference`(在 [10 章](10-testing.md))如何验证 SIMD 正确性。

## 下一章

[10 测试与属性测试](10-testing.md):`#[test]`、doctest 与 `proptest`。
