# 08 L6 打磨层:量化、两阶段检索与 async 门面

> **本章目标**:在不损失实用召回的前提下,把查询的内存带宽(决定性瓶颈)砍掉 4 倍,
> 并完成 async 门面与性能收尾。
> **前置阅读**:[03 §4.3](03-l1-memory.md)(带宽下限分析)、[05 §6](05-l3-hnsw.md)(查询复杂度)、[02 §4](02-l0-core.md)(SIMD)。
> **本章你将学到**:为什么带宽是瓶颈 → i8 标量量化的公式与误差分析 → f16 →
> 两阶段检索(粗排 + 重打分)→ 自动回退 → async 门面。
>
> **落地状态(2026-09)**:**本章已落地**——`src/quant/` 提供 i8 标量量化(段级
> 每维 `(v_min,v_max)`)、f16(feature `quant-f16`)、两阶段候选预算与建段召回抽样;
> `vsec` 段格式升 `0x0005`,qvec 区紧随 norm 区(布局见 04 §2.1 与
> `FC-QUANT-POST-002`);两阶段检索、建段抽样回退(I13)、`stats().quant`、
> `feature = "async"` 门面均已接线,`FC-QUANT-*` 全部转 `Passed`。
> 与本章原文的三处实现取舍(以契约为准):
>
> 1. **图仍由 f32 构建,量化副本只服务查询期打分**——装配式 HNSW 的图结构在
>    flush 期一次性构建,查询期 `score_query` 读 qvec 副本算粗排分;这样 f32 图
>    天然作为建段召回抽样的对照基线,且构建期成本不变(带宽收益在查询期兑现);
> 2. **i8 粗排内核用「u8 码位零扩展 + FMA」而不是 `maddubs`/VNNI**——每维独立
>    scale 无法直接喂给整数点积指令;`core::simd::dot_u8_f32` 每行只读 `d` 字节
>    (带宽 ÷4),AVX2 可用时走 `_mm256_cvtepu8_epi32` + FMA,否则回退标量;
> 3. **每维参数表紧随 norm 区之后写入 qvec 区**(vsec 头仍是定长 64 B),而非
>    扩长头部;`VsecView::quant_row` 提供零拷贝切片,`quant_params` 按需解码参数表。
>
> 量化副本属**持久段特性**:纯内存库配置量化在构造期返回 `Unsupported`
> (`FC-QUANT-ERR-002`);`src/quant/` 为纯原语模块(无 I/O/锁/全局态,依赖等级同
> L0),供 L2 段编码与 L3 索引打分直接复用。`benches/quant.rs` 给出 f32/i8 的
> 微缩对照(4k×512);微缩规模下每查询的候选收集/位图等固定开销占比高,≥3×
> 加速门槛需 1M×1536 heavy 档,与冷启动、fuzz 长跑同属 CI 收尾(14 §4/§5)。

模块:`quant/{scalar_i8.rs, f16.rs, rescore.rs, support.rs}`(已落地)、`memory::async_facade`(feature `async`)、`src/fuzzing.rs`(feature `fuzzing`)

---

## 1. 为什么量化:瓶颈在带宽,不在算力

[03 §4.3](03-l1-memory.md) 给出过账本:暴力 1M×1536 维查询要读 ~6GB,
内存带宽 ~50GB/s → 下限 ~120ms;HNSW 把"读多少向量"降到 ~千个,
但**每字节仍是 4 字节 f32**。带宽账本量化前后:

| 方案 | 粗排副本每向量字节 | 1M 条副本总量 | 同带宽可探查行数 |
|---|---|---|---|
| f32 | $4d$ = 6144 B | 6.1 GB | 1× |
| f16 | $2d$ = 3072 B | 3.1 GB | 2× |
| **i8** | $d$ = 1536 B | **1.5 GB** | **4×** |

量化 = 用更少的位表示每个分量。SIMD 通道不变、指令数不变,
**每条指令搬运的字节减半/减 4 倍**——粗排延迟近似同比例下降(经验值)。

> **副本 ≠ 总存储**:上表是**粗排副本**的大小(决定查询带宽)。两阶段检索要求精排回退到
> 原始 f32([08 §4](08-l6-quant.md)),故 f32 原向量**始终保留**;开启量化后每行总存储
> = `4d`(f32 原向量)+ 副本(i8 为 `d`、f16 为 `2d`),即 i8 模式约 `5d` B/行、f16 约 `6d` B/行。
> 量化换来的是**查询带宽 ÷4**,不是磁盘占用 ÷4;容量估算见 [16 §11](16-api-reference.md)。

---

## 2. i8 标量量化:`scalar_i8.rs`

### 2.1 【直觉】千分尺改厘米尺

f32 像千分尺,记录每个分量到小数点后 7 位;i8 像厘米尺,只记 256 个刻度。
嵌入向量的分量本来就带着模型噪声(第 4 位有效数字已经不可信),
用厘米尺量它们,排序几乎不变——这就是量化的可行性来源。

### 2.2 【数学】编码与误差

对**每段每一维**独立量化(段内统计,非对称区间):

$$\Delta = \frac{v_{\max} - v_{\min}}{255}, \qquad q = \mathrm{round}\!\left(\frac{x - v_{\min}}{\Delta}\right) \in [0, 255], \qquad \hat{x} = v_{\min} + q\,\Delta$$

存储:`q` 本身 1 字节(按 0–255 无符号编码存储;查询侧以 u8 零扩展 + FMA 加权,
见 §2.3——每维独立 scale 无法直接喂给 `maddubs`/VNNI 类整数点积指令;对外统称
"i8 量化")
+ 段头 $2 \times d$ 个 f32 的 $(v_{\min}, v_{\max})$ 表
(1536 维共 12KB/段,均摊到每行可忽略)。

**误差上界**:`round` 的舍入误差 ≤ Δ/2,故 $|x - \hat{x}| \le \Delta/2$。
下面先分析**只量化库侧、查询向量保持 f32** 的情形(§2.3 会说明查询侧也量化的
全整数模式,其额外偏差同样由 §4 的 f32 重打分兜底):

$$|q\cdot v - q\cdot \hat v| \;\le\; \sum_i |q_i| \, |v_i - \hat v_i| \;\le\; \frac{\Delta}{2}\,\|q\|_1$$

**【算例】** 典型嵌入分量分布 $\approx [-1, 1]$:$\Delta = 2/255 \approx 0.0078$,
最大绝对误差 0.0039。单分量看微不足道;1536 维求和的最坏界
$\frac{\Delta}{2}\|q\|_1 \approx 0.0039 \times 1536 \approx 6$(最坏情况所有分量误差同号叠加,
此时各分量量级为 1、$\|q\|_1 \approx 1536$)——但实际误差**符号随机**,
均方根(RMS)才代表现实:

$$\mathrm{RMS}(\text{dot-product error}) = \frac{\Delta}{\sqrt{12}}\cdot\|q\|_2
= \frac{2/255}{\sqrt{12}}\cdot\|q\|_2$$

对单位查询向量($\|q\|_2 = 1$),RMS ≈ 0.0023;若分量量级为 1
($\|q\|_2 \approx \sqrt{1536} \approx 39$),RMS ≈ 0.088。排序语义下这一量级的
典型抖动对"谁是前 10 名"的扰动很小——但不是零,这正是 §4 需要**重打分**的原因:
量化分只做粗排,精排回到 f32。

### 2.3 【工程】量化点积的 SIMD

量化后分量是整数,查询向量按**每个段各自的 min/max 表**转换为粗排权重(不同段刻度
不同,不能全局只算一次)。实现口径(落地状态 §1):查询侧保持 f32,逐维预计算
`w_i = q_i·Δ_i` 与偏置 `b = Σ q_i·v_min_i`,每行只需 `b + Σ code_i·w_i`——
码位零扩展到 f32 后 FMA(`core::simd::dot_u8_f32`,AVX2 下
`_mm256_cvtepu8_epi32` + `_mm256_fmadd_ps`),读侧每行也只要 `d` 字节。
之所以不用 `maddubs`/VNNI:每维独立 scale 无法直接喂给整数点积指令,
零扩展 + FMA 保持了相同的带宽收益且可移植。不同段的 min/max 不同,
**跨段比较必须用同一折算口径**(`Metric::score_from_dot`)。

> **查询不落码位**:查询侧不重新编码为 u8,而是以 f32 参与逐维加权,避免钳制
> 偏差叠加;粗排分数只用于选候选,精排仍回原始 f32(§4)。

### 2.4 复杂度与收益

| 项 | f32 | i8 |
|---|---|---|
| 单次点积(读侧带宽) | $4d$ B | $d$ B(**4×**) |
| 粗排副本存储 | 不单独存 | $d$ B/行 + $2d$ 个 f32/段 |
| 段总存储(含 f32 原向量) | $4d$ B/行 | $5d$ B/行 + $2d$ 个 f32/段 |
| 精度 | 基准 | RMS 误差 ≈ 0.09(1536 维、分量量级 1 即 ‖q‖₂≈39 时;归一化查询约 0.0023,经验值) |

---

## 3. f16 量化:`f16.rs`(feature `quant-f16`)

IEEE 754 half:1 位符号 + 5 位指数 + 10 位尾数。相对精度 $2^{-11} \approx 4.9\times10^{-4}$,
范围 ±65504——对归一化附近的嵌入向量绰绰有余。**优点**:无需 min/max 表、
无跨段刻度问题、误差无偏;**缺点**:只省 2 倍(对比 i8 的 4 倍)。
定位:i8 的**保守替代**(对误差敏感的库)。
**F16 同样走两阶段重打分**([08 §4](08-l6-quant.md)):粗排用 f16 副本,精排回 f32——
因此 `Hit.score` 的口径与 i8 一致(不变量 I12),不是"f16 分数直接返回"。
`half` crate 只在 `quant-f16` 下编译;**未开启该 feature 时**
`Builder::quantization(VectorFormat::F16)`(以及打开含 f16 段的库)返回
`Unsupported { feature: "quant-f16" }`,绝不静默降级(`FC-QUANT-ERR-001/002`,
已落地);`F32` 与 `I8Rescored` 不依赖任何 feature。

---

## 4. 两阶段检索:`rescore.rs`

### 4.1 【直觉】海选 + 面试

量化分数有噪声,直接按它取 top-k 可能错杀第 10/11 名这类边界者。
正确姿势分两步:**海选**(量化副本上放宽取 $4k$ 个候选,噪声把边界者挤进挤出的
概率都被 4 倍余量吸收)→ **面试**(对这 $4k$ 个候选,从 vsec 原始 f32 重算精确分,
取真 top-k)。精确计算只发生在千级候选上,总带宽仍是量化的 4 倍优势。

### 4.2 流程与复杂度

```text
粗排: 量化副本 HNSW → TopK(4k)           带宽 = 4k × d 字节(i8)
精排: 对候选取 f32 原向量(同段 vsec)重打分 → TopK(k)   带宽 = 4k × 4d 字节(可按需分批)
      # f32 原向量与量化副本并存:副本只服务粗排带宽,精排始终回退 f32(I12)
```

精排带宽 $16kd$ 字节看似回到 f32——但只对 $4k$(≈40–80 个)候选,
远小于搜索途中本要路过的千级节点;且 vsec 本就 mmap 常驻页缓存(刚被粗排 locality 洗过),
实际读放大很小(经验值)。总收益:查询延迟 ~3–4×,召回损失 ≤ 2%(门槛,见下)。
候选倍率由 `Tuning.rescore_oversample` 配置(默认 4,即 $4k$;需 ≥ 1),
粗排候选数与候选总数取小。

### 4.3 召回门槛与自动回退

- 落地口径:flush/compaction **建段时**抽样自查询(等距抽样 `RECALL_SAMPLE_QUERIES`
  条),以同一 f32 图的 top-10 为参照计算"量化粗排 + f32 精排"的一致率;
  低于 `Tuning.quant_recall_floor`(默认 0.98)即**该段不写 qvec、回退 f32**,
  且 `stats().quant` 的 `configured`/`active`/`recall_est` 如实反映(I13,
  验收见 [14 §3.2](14-testing.md));`quant_recall_floor = 0.0` 关闭回退,
  `> 1` 恒回退(测试用)。离线 1 万查询基准与 1M×1536 门槛仍属 CI 收尾
  ([14 §4](14-testing.md));
- 运行期监控:`stats().quant.recall_est` 暴露建段抽样一致率(各段取最小值;
  重开库后无建段采样上下文,为 `None`,回归见
  `tests/l6_contracts.rs::i8_qvec_roundtrip_after_reopen` 与 [16 §1.6](16-api-reference.md))。

> **常见误区**:① 以为开量化后磁盘变小——f32 原向量始终保留,省的只是**查询带宽**;
> ② 以为 `Hit.score` 是量化分——始终是 f32 精排分(I12);
> ③ 配置了 `VectorFormat::F16` 却未开 `quant-f16` feature——构造期或打开期即返回
> `Unsupported`,不会静默降级(`FC-QUANT-ERR-001/002`)。

---

## 5. 副本生成与管理

- **存储布局**:量化副本以 `qvec` 区追加在 norm 区之后、删除位图之前(`vsec` 头
  `quant != 0`);i8 先写段级每维 `(v_min, v_max)` 交错表(`2d` 个 f32),再写每行
  `d` 字节码;f16 只有每行 `2d` 字节码。f32 原向量保持在 `vec` 区,二者同一
  `SegmentId`、同 CRC、同生同灭([04 §2.1](04-l2-persist.md)、`FC-QUANT-POST-002`);
- flush 生成段时**同步**建量化副本(构建期一次性成本,与倒排同批);
- 旧段升级(用户中途开量化):当前实现不在后台回填,由下一轮 compaction 按当前
  配置重写(格式迁移零特殊逻辑);未重写前旧段继续以 f32 服务,`stats()` 逐段可见;
- compaction 合并时以**当前配置**的量化格式重写副本(复用 [07 §4](07-l5-life.md)
  流程,`FC-QUANT-POST-003`)。

---

## 6. async 门面(memory::async_facade.rs)

```rust
// 同步与异步方法不能同名挂在同一类型上(Rust 方法名唯一),故异步门面是
// 轻量包装类型 AsyncNamespace;它共享底层句柄与写锁,`insert().await` 即其方法。
#[cfg(feature = "async")]
pub struct AsyncNamespace { inner: Namespace }

#[cfg(feature = "async")]
impl Namespace {
    pub fn into_async(self) -> AsyncNamespace { AsyncNamespace { inner: self } }
}

#[cfg(feature = "async")]
async fn run_blocking<T, F>(task: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .expect("async 门面:阻塞任务异常终止") // 核心不 panic,见 L0 契约;属文档化例外
}

#[cfg(feature = "async")]
impl AsyncNamespace {
    pub async fn insert(&self, rec: Record) -> Result<InsertOutcome> {
        let inner = self.inner.clone();             // Arc 克隆,无生命周期问题
        run_blocking(move || inner.insert(rec)).await
    }
}
```

- **核心零 tokio**:所有 async 方法 = `spawn_blocking(同步方法)` 的机械包装,与同步 API
  同语义(I14);`AsyncNamespace` 亦 `Send + Sync`;
- **覆盖范围**:`AsyncNamespace` 包装 `Namespace` 的阻塞入口(`insert` / `insert_batch` /
  `update` / `get` / `get_many` / `get_vector` / `exists` / `delete` / `touch` / `relate` /
  `consolidate` 等);点读方法返回 owned [`StoredRecord`](../design/16-api-reference.md)
  (含 `RowId`,同步版借用 `RecordRef` 无法跨线程移动),`StoredRecord::into_record`
  可转回可写 `Record`;`search()` 构建器本身是轻量纯内存操作,而 `execute()` 是阻塞调用,
  异步场景请对 `execute()` 用 `spawn_blocking` 包装(库不额外提供异步 `SearchBuilder`);
  `Mneme` 级操作(`flush` / `close` / `backup_to` / `snapshot`)同样不属于 `AsyncNamespace`,
  需要异步调用时由宿主用 `spawn_blocking` 包装或直接调同步版(它们本就是毫秒级或纯内存);
- 为什么不用真正的异步 I/O?WAL 顺序写是**微秒级内存追加 + 毫秒级 fsync**,
  fsync 没有有意义的异步形态(`tokio::fs` 底层也是阻塞线程池);
  `spawn_blocking` 语义清晰、不阻塞 reactor、成本一次线程池调度(微秒);
- 嵌入型库的调用方通常自己持运行时,Mneme 不选 runtime、不强制 feature——
  同步版永远是零成本可用的。

---

## 7. 收尾清单

| 事项 | 标准 | 详见 |
|---|---|---|
| criterion 基准集 | 已落地:建库吞吐(`benches/hnsw.rs`)与量化微缩对照(`benches/quant.rs`);QPS-ef 曲线 / 过滤三档 / 混合 / compaction 停顿 / 冷启动待 CI | [14 §4](14-testing.md) |
| cargo-fuzz | 五目标骨架已搭起(`fuzz/`);1h/24h 长跑待 CI | [14 §5](14-testing.md) |
| MSRV | `rust-version = 1.93`(edition 2024) | Cargo.toml |
| 文档 | pub API 100% 文档覆盖;`#![deny(missing_docs)]` | — |
| 发布 | `cargo publish --dry-run` + 变更日志(发布前执行) | — |

---

## 8. 层边界契约(L6 → 外部)

**向上提供**:量化配置(`VectorFormat::F32 / F16 / I8Rescored`)、自动回退保证、
async 门面(与同步 API 完全同语义)。

**依赖**:L0(SIMD)、L2(vsec 读)、L3(带位图 ANN)。

**不变量**:

- I12 量化模式下返回的 `Hit.score` 一律为 **f32 精排后的分数**(调用方无感);
- I13 召回门槛不达标 → 自动回退 f32,且 `stats()` 可见;
- I14 async 与 sync API 对同一状态的操作序列产生等价结果(共享同一把写锁)。

---

## 本章小结

- 瓶颈是**内存带宽**;量化只降查询带宽、**不缩磁盘**(f32 原向量始终保留供精排)。
- i8 编码公式与误差界;f16 是保守替代;两阶段检索 = 4k 粗排 + f32 精排。
- 召回门槛不达标自动回退 f32(I13);`Hit.score` 始终是 f32 精排分(I12)。
- async 门面 = `spawn_blocking` 机械包装,与同步 API 同语义(I14);核心零 tokio。

## 下一章

[09-memory-model.md](09-memory-model.md):在打磨好的引擎之上,加入关系、双时态与记忆沉淀。
