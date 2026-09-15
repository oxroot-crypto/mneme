# 02 L0 原语层:类型、距离数学与基础算法

> **本章目标**:定义全库的"地基"——没有任何 I/O、没有全局状态,只有类型与纯函数。
> 后面每一层都直接复用本章。
> **前置阅读**:[00 §3–§4](00-fundamentals.md)(嵌入向量与相似度)、[00 §7](00-fundamentals.md)(大 O)。
> **本章你将学到**:ID/错误设计 → 距离度量的完整数学与 SIMD 实现 → TopK 堆 → varint 编码。

模块清单:`core/{types.rs, error.rs, metric.rs, simd/, varint.rs, meta.rs, heap/, bitset.rs, text.rs, options/}`
(`options/` 为按主题拆分的模块目录,见 §8)

---

## 1. ID 与类型系统

六类标识符,全部用 **newtype 模式**(用单字段结构体包住整数;`Key` 包住字符串)——
编译器从此能区分"这个 u64 是行号还是序号",混用即编译错误:

| 类型 | 角色 / 范围 | 含义 | 生命周期 |
|---|---|---|---|
| `RowId(u64)` | **全局稳定逻辑标识**,首次写入时分配 | 公开 API 的稳定句柄(`Hit.rowid`、`get_by_rowid`);访问统计、关系边均以它为主键;更新/upsert **保留 RowId**(写新物理版本) | 永不复用;跨更新、跨段、跨 compaction 稳定不变 |
| `SlotId(u32)` | **内部**段内物理槽位(每物理版本一个) | vectors 下标 / 删除位图 bit / HNSW 节点 id;仅库根 re-export 类型名,公开签名不暴露其数值,调用方不得依赖 | 段内追加写、永不复用;仅 compaction 重建新段时按新段重新编号(见 [03 §3](03-l1-memory.md)) |
| `SeqNo(u64)` | 全局提交序号,单调递增 | MVCC 快照的基石([00 §6.6](00-fundamentals.md)) | 永不复用 |
| `SegmentId(u32)` | 段文件编号 | 与文件名 `seg_000042.vsec` 对应 | 永不复用 |
| `NsId(u32)` | 命名空间编号 | `(NsId, Key)` 是复合主键;WAL 帧、key_index 均引用 | 永不复用 |
| `Key(Arc<str>)` | 用户外部键(可选) | 应用层的记忆 id | — |

**为什么把"记录标识"与"物理槽位"分开?** `RowId` 是应用可见的稳定身份:即便后台
compaction 重排了物理位置,`get_by_rowid` / `Hit.rowid` / 访问统计仍然有效。
`SlotId` 是段内物理位置,HNSW 图的边直接存 `SlotId`(u32,省空间、段内局部);
段合并重建时 `SlotId` 重新分配,但 `RowId` 保持不变。若让段内槽位直接对外,
一旦 compaction 重建段、`SlotId` 被重新分配,外部持有的旧槽位就会指向错误的记录
——这是超长期系统的典型取舍:**用一点空间与一次映射换永久的不变量**。

**不变量 I22(稳定逻辑标识)**:`RowId` 跨 `update`/upsert 不变,访问统计、关系边与
`get_by_rowid` 始终以它为主键,跨更新、跨段、跨 compaction 有效(验收见
[14 §2.3](14-testing.md),映射见 [16 §9](16-api-reference.md))。

---

## 2. 错误设计

`thiserror` 派生的统一错误枚举;对外是唯一错误类型,跨层传递零成本
(枚举体积 = 最大变体大小,无装箱):

```rust
#[non_exhaustive] // 预留新增变体而不破坏下游穷尽匹配
pub enum MnemeError {
    Io(#[from] std::io::Error),
    Corrupted { segment: Option<SegmentId>, reason: String }, // CRC 不过/魔数不符;None = 文件级损坏(如 MANIFEST 全坏)
    DimensionMismatch { expected: u32, got: usize },    // 建库时已锁维度
    MetricMismatch { existing: Metric, requested: Metric }, // 打开时参数与库不符
    KeyMismatch { expected: Key, got: Key },            // supersede 新记录 key 与目标冲突
    KeyNotFound(Key),                                   // 预留变体,任何 API 不产生(见 16 §4)
    DuplicateKey(Key),                                  // InsertMode::RejectDuplicate 时
    FilterParse(String),                                // DSL 语法错误,带位置信息
    Busy(&'static str),                                 // 独占锁被占/备份中
    TooLarge { field: &'static str, limit: usize, got: usize }, // 数据超限额,见 16 §8
    LimitExceeded { field: &'static str, limit: usize, got: usize }, // 参数越上限(维度/top_k/ef)
    MetaTooDeep { limit: usize, got: usize },           // metadata 嵌套过深
    UnsupportedVersion { file: &'static str, found: u16, max: u16 }, // 文件格式与当前定义不一致
    Closed,                                             // 库已关闭后经任意句柄读写
    NonFinite,                                          // 向量分量或标量因子 NaN/±Inf
    Config { reason: &'static str },                    // 建库/查询配置或策略参数非法
    Unsupported { feature: &'static str },              // 与 feature 门控/形态不符的能力,绝不静默降级
    Inconsistent { reason: &'static str },              // 内部不变量被破坏
}
pub type Result<T> = std::result::Result<T, MnemeError>;
```

设计约定:错误信息面向**排查**——`Corrupted` 必须带段号(文件级损坏时为 `None`)与原因;
`FilterParse` 必须带出错位置;不在错误里嵌套第二层错误类型(避免错误地狱)。
**每个语义类都有专属变体**(错误分类矩阵见 [spec/contracts.md §0.2](../spec/contracts.md)),
禁止用泛化变体承载多种失败。各错误的可重试性与处置建议见 [16 §4](16-api-reference.md)。

---

## 3. 距离度量:`metric.rs`

三种度量 `Metric::{Cosine, Dot, Euclidean}`,统一接口:

```rust
pub enum Metric { Cosine, Dot, Euclidean }
impl Metric {
    /// 原始分数:Dot/Cosine 越大越相似;Euclidean 返回距离平方(越小越近)。
    /// 方向由 better() 统一——所有 TopK、归并与排序都必须经 better() 比较,
    /// 绝不直接比较 score 的数值大小。
    pub fn score(&self, a: &[f32], b: &[f32], a_norm: f32, b_norm: f32) -> Score;
    pub fn better(&self, x: Score, y: Score) -> bool;  // 归一比较方向:true = x 更优
    pub fn needs_norm(&self) -> bool;                  // Cosine/Euclidean 需要 norm 列(仅 Dot 不需要)
}
pub type Score = f32;
```

**背景回顾**(详见 [00 §4](00-fundamentals.md)):点积 `a·b`、
余弦 `cosθ = a·b/(‖a‖·‖b‖)`、欧氏距离 `‖a−b‖`。
本节讲工程化的三个关键点。

### 3.1 【工程】欧氏距离复用点积核

展开恒等式:

$$\|\mathbf{a}-\mathbf{b}\|^2 \;=\; \|\mathbf{a}\|^2 + \|\mathbf{b}\|^2 - 2\,\mathbf{a}\cdot\mathbf{b}$$

$\|\mathbf{a}\|^2$ 在写入时算好存进 norm 列(每向量 4 字节),查询时
$\|\mathbf{q}\|^2$ 是常数,于是**三种度量全部归结为一次点积**(norm 列存的是**范数平方**):

| 度量 | 查询时的实际计算 |
|---|---|
| Dot | `a·b` |
| Cosine | `a·b / sqrt(a_norm * b_norm)`,库侧 `a_norm = ‖a‖²` 预计算、查询侧 `b_norm = ‖b‖²` 每次查询现算一次 |
| Euclidean | `‖a‖² + ‖b‖² − 2*a·b` |

结论:**SIMD 层只需一个极致优化的点积内核**(§4)。代价:欧氏/余弦度量下每个向量
多存 4 字节 norm(1536 维向量 6KB,开销 0.07%,可忽略)。

### 3.2 【工程】余弦不预归一化

也可以在写入时把向量归一化、余弦退化为点积,但 Mneme **不这么做**:
原始向量必须原样保留(宿主可能要取回原文向量做重排/可视化),
归一化副本会占双倍空间。实时计算 `a·b / sqrt(a_norm * b_norm)`(库侧预存 + 查询侧现算)
只多一次乘法、一次开方与一次除法,可忽略。

### 3.3 数值稳定性

- 分母下限保护:`‖a‖·‖b‖ < ε`(如 1e-12,零向量)时余弦返回 0,不返回 NaN;
- **非有限值在入口拒绝**:`insert` 时校验每个分量为有限值,`NaN`/`±Inf` 返回
  `NonFinite`(见 [03 §2.1](03-l1-memory.md)、[16 §8](16-api-reference.md));
  `importance`/`confidence`/边权/`boost` 等标量因子同口径;
  距离函数本身不做该检查,以保持内层循环零分支;
- 点积用 f32 累加即可(嵌入分量量级 ~0.1,1536 维累加误差远小于嵌入模型自身噪声);
  不用 Kahan/双精度——索引场景要的是**排序稳定性**而非绝对精度,
  且全库统一实现保证"同一个查询,同样的排序"。

### 3.4 复杂度

| 运算 | 标量 | SIMD(AVX2,8 宽) | 备注 |
|---|---|---|---|
| 点积(单对,d 维) | $O(d)$ 次乘加 | $\approx O(d/8)$ 次向量指令 | 内存带宽:两向量全冷需读 $8d$ 字节;扫描场景查询向量驻留缓存,每候选 ≈ $4d$——这是真正的下限 |
| 范数 | $O(d)$ | 同上 | 写入时一次,存 norm 列 |

---

## 4. SIMD:`simd/`

### 4.1 【直觉】什么是 SIMD

普通(CPU)指令一次处理一个数;**SIMD(Single Instruction, Multiple Data,
单指令多数据)** 一次处理一排数:AVX2 指令集的一条 `vfmadd`(乘加)指令,
同时对 **8 个 f32** 做乘加。算 1536 维点积,标量要 1536 次乘加,
AVX2 只要约 192 条 FMA 指令(仅计乘加,不含两条向量加载;含加载约 576 条,见 §4.3)+ 少量归约。
这是"暴力扫描也能跑进毫秒"的物理基础。

类比:点积是"两列数字逐位相乘再竖着加总"。标量是单人计算器按 1536 次;
SIMD 是 8 台计算器并排,每次同时对齐 8 对数字。

### 4.2 【工程】实现策略:运行时分发,手写四种内核

```rust
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "点积要求两向量等长");
    #[cfg(target_arch = "x86_64")]
    {
        // AVX2 内核用 _mm256_fmadd_ps,故需同时具备 avx2 与 fma;否则回退 SSE2。
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            // SAFETY: 已确认 avx2 与 fma 均可用。
            unsafe { x86::dot_avx2(a, b) }
        } else {
            // SAFETY: x86_64 基线保证 SSE2 可用。
            unsafe { x86::dot_sse2(a, b) }
        }
    }
    #[cfg(target_arch = "aarch64")]
    { unsafe { neon::dot_neon(a, b) } }              // aarch64 必有 NEON
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    { dot_scalar(a, b) }
}
```

- 内核手写 `std::arch`(Rust 标准库的 intrinsics),**不引入任何依赖**;
- f32x8 主循环 + 尾部标量收尾(维度不是 8 的倍数时);
- **非对齐加载**:内核用 `loadu` 读取,**不要求调用方保证 32B 对齐**(避免未对齐
  输入触发 UB);存储层把向量按 32B 摆放(见 [04 vsec](04-l2-persist.md))只是
  进一步避免跨缓存行惩罚的优化,而非正确性前提;
- NEON 是 128 位(4 宽),同样的代码结构、更少的通道;AVX-512F/AVX2/SSE2/NEON
  四个内核各自
  独立实现,统一通过与可移植标量参考 `dot_scalar` 的对照测试验证等价性
  (FC-CORE-INV-001)。

### 4.3 【数学】点积内核的指令数

AVX-512F 一次 `_mm512_fmadd_ps` 完成 16 对乘加、AVX2 一次 `_mm256_fmadd_ps`
完成 8 对(乘完即加进累加器);i8 粗排另有 AVX-512/AVX2 的 `u8×f32` 内核。
AVX-512F 仅在向量长度 ≥ 256 时启用:短向量下宽内核收益不足、且受宽指令频率影响
(实测 64 维基准变慢)。$d$ 维点积:

- 主循环:$\lceil d/8 \rceil$ 次迭代,每次 1 条 FMA + 2 条加载(两条向量各一条);
- 收尾:8 分量的水平归约(HADD)约 $\log_2 8 = 3$ 步。

总指令数 $\approx 3d/8 + 3$。**复杂度仍是 $O(d)$,但常数缩小 8 倍**;
真实瓶颈转为内存带宽(每对向量 $8d$ 字节必须读出),这也是第 [08 章](08-l6-quant.md)
量化(字节数减 4 倍)能再提速约 4 倍的根本原因。

### 4.4 【算例】8 宽手算

`a = [1,2,3,4,5,6,7,8]`,`b = [1,1,1,1,1,1,1,1]`:

```
FMA:      8 个通道同时各得 a_i·b_i → [1, 2, 3, 4, 5, 6, 7, 8]
归约 3 步: [1..4]+[5..8] = [6, 8, 10, 12] → [16, 20] → 36 = 1+2+...+8 ✓
```

---

## 5. TopK 有界堆:`heap/`

检索的终点永远是"从 N 个分数里挑最大的 k 个"。比完全排序更聪明的做法:

### 5.1 【直觉】只保留"当前前 k 名"

维护一个只装 k 个元素的小根堆(根 = 当前第 k 名,即"守门员"):
来一个新分数,若连守门员都打不过,直接扔掉;打得过,挤掉守门员再重新站岗。
全程只需容纳 k 个分数的内存,而不是给 N 个分数排序。

> **方向由 `Metric::better` 决定**:堆里"谁是最差"一律走 `better()`,不直接比较
> 数值。因此对 Dot/Cosine 表现为小根堆(保留最大分),对 Euclidean(返回距离平方)
> 表现为等价的"保留最小距离"——语义始终是"保留最优的 k 个"。

**同分次级键**:两分数经 `better()` 互相打不出优劣时,按**载荷升序**决定去留与输出
顺序,故载荷类型需实现 `Ord`(即 `TopK<T: Ord>`)。这保证同分结果的集合与顺序同
插入顺序无关、可确定性复现;`into_sorted_vec` 按"最优在前、同分载荷升序"输出。
(载荷比较须为 $O(1)$ 才能维持下述 $O(1)$ 拒绝路径——实际载荷为 `RowId` 等整数。)

### 5.2 【数学】正确性与复杂度

- 小根堆性质:任意父节点在 `better()` 意义下不优于子节点(同分按载荷升序的次级键,
  见 §5.1),故**根是堆中最差者**(守门员);
- `push` 为 $O(\log k)$(沿树高上浮/下沉,树高 $= \log_2 k$):未满时插入并上浮;
  已满时只与守门员比较,更优则替换根并下沉,否则 $O(1)$ 直接丢弃;

$$T(N) \;=\; k \cdot O(\log k) \;+\; (N-k) \cdot O(1) \;+\; m \cdot O(\log k), \qquad m \le N-k$$

其中第一项是**前 $k$ 次插入**的上浮;第二项是已满后每次推入的常数时间比较
(打不过守门员即丢弃,该 $O(1)$ 路径占绝大多数);第三项是已满后 $m \le N-k$
次替换的下沉。

$$\Rightarrow\; T(N) = O(N \log k), \quad S = O(k)$$

对比排序的 $O(N \log N)$:$N = 10^6, k = 10$ 时,$N\log_2 k \approx 3.3\times10^6$,
只有 $N\log_2 N \approx 2\times10^7$ 的约 1/6;何况堆的 $O(1)$ 常数路径(拒绝)占绝大多数,
且空间 $O(k)$ vs $O(N)$;并行分块时每块只需各自的 TopK(见下)。

### 5.3 【算例】k=3,N=6

分数流 `[5, 9, 1, 7, 3, 8]`:

```
push 5 → 堆 {5}            push 9 → {5,9}          push 1 → {1,5,9}
7: 7 > 根1 → 换 → {5,7,9}   3: 3 > 根5? 否 → 丢      8: 8 > 根5 → 换 → {7,8,9}
结果 top3 = {9,8,7} ✓
```

### 5.4 【工程】并行归并单元

`TopK<T: Ord>`(同分次级键见 §5.1)实现两个方法:`push(score, payload)` 与 `merge(Self)`。
并行暴力扫描([03 §3](03-l1-memory.md))时,每个线程块持有独立 TopK,
最后 k 路归并:`merge` 代价 $O(k \log k)$,与 $N$ 无关——这是扫描可线性扩展的前提。

---

## 6. varint:`varint.rs`

### 6.1 【直觉】小数字就该占小空间

固定 8 字节存一个 u64,但检索场景里大量整数是**小**的:倒排表的 rowid **差分**
(后一个 rowid − 前一个,往往是个位数)、记录长度、块内计数……
**varint(变长整数)** 让 0–127 只占 1 字节、128–16383 占 2 字节,以此类推,
每个字节用最高位当"还有后续"的标志(continuation bit)。

### 6.2 【数学】编码规则

把整数的二进制按 **7 位一组**从低位切分,**低位组在前**;除最后一组外,
每组所在的字节最高位置 1:

$$\text{encode}(x):\; b_i = \begin{cases} 0x80 \mid (\text{low-7-bit group}_i) & i < \text{last group} \\ \text{low-7-bit group}_i & i = \text{last group} \end{cases}$$

**算例**:encode(300)。300 = `0b100101100`(9 位)→ 按 7 位切:
低组 `0101100`(44),高组 `0000010`(2)。

```
第 1 字节 = 44 | 0x80 = 0xAC   (有后续,最高位置 1)
第 2 字节 = 2        = 0x02   (最后一组,最高位 0)
300 → [0xAC, 0x02];解码:44 + (2 << 7) = 44 + 256 = 300 ✓
```

### 6.3 复杂度

| 操作 | 时间 | 空间 |
|---|---|---|
| 编码/解码 u64 | $O(\lfloor \log_{128} x \rfloor + 1) \le 10$ 字节操作 | 小整数(多数)1–2 字节;最坏 10 字节 |

Mneme 用途:倒排表 `SlotId` 差分([06 §3](06-l4-query.md))、段内变长字段长度前缀。
配合差分排序序列,期望压缩到原大小的 20–40%(经验值)。

---

## 7. meta.rs:serde 隔离区

全库只有这个文件允许出现 `serde` 类型;其余代码只见别名与辅助函数:

```rust
pub type Meta = serde_json::Value;                     // 元数据 = 任意 JSON
pub use serde_json::json;                              // 仅重导出 json! 宏,便于构造 Meta;不暴露其它 serde_json 类型
pub fn get_path<'v>(v: &'v Meta, path: &str) -> Option<&'v Meta>;   // "a.b.c" 点路径
pub fn as_f64(v: &Meta) -> Option<f64>;   // as_i64 / as_bool / as_str / as_ts 同理
```

- `Meta` 选择 JSON Value 而非强类型 schema:Agent 的记忆元数据是**开放的**
  (不同应用记不同字段),schema-free 是需求,不是妥协;
- 代价:每条多几十字节与解析开销;缓解:字段字典 + zone map 前推(见
  [04 §5](04-l2-persist.md)),查询不解析整条 JSON;
- 替换性:若要换自研 JSON,只改此文件。

**复杂度**(FC-CORE-CPLX-006):`get_path` 按 `.` 分段逐级下降,每级一次
`Value::get`(map/数组索引,平均 $O(1)$),时间 $O(p)$($p$ = 分段数);
`as_f64/as_i64/as_bool/as_str/as_ts` 为单次类型匹配,时间 $O(1)$、空间 $O(1)$。

---

## 8. options 模块:全局参数

> 模块目录按主题拆分:`dimension.rs`(维度)、`write.rs`(fsync 策略 / 插入模式 /
> 更新补丁 / 压缩)、`index.rs`(HNSW / 调参 / 量化格式)、`limits.rs`(限额)、
> `lifecycle.rs`(compaction)、`scoring.rs`(打分 / 反馈 / 关系)、`clock.rs`(时间源);
> 子模块保持私有,类型经 `options/mod.rs` 统一 re-export,公共 API 路径不变。

```rust
pub struct Dimension(u32);        // 1..=65536;Builder 入口接受 u32 并校验后转为 Dimension,内部不再用裸整数
pub enum FsyncPolicy { Always, Batched(Duration), OnFlush, Never }  // Never 仅供测试
pub enum InsertMode { Upsert, RejectDuplicate }   // 同 key 行为,默认 Upsert
pub enum VectorFormat { F32, F16, I8Rescored }    // 量化格式,L6
pub struct HnswParams { m, m0, ef_construction, ef_search }         // L3
pub struct CompactionPolicy { tier_ratio, tier_count, dead_ratio, wal_bytes, wal_file_bytes, segment_rows, io_budget, history_horizon } // L5
pub struct Tuning { parallel_block, field_dict_max, bloom_fpp, brute_force_max_rows, filter_post_threshold, filter_brute_threshold, stopwords, rescore_oversample, quant_recall_floor } // 进阶;后两项 L6(见 08/16)
pub struct Limits { key_bytes, text_bytes, meta_bytes, meta_depth, ns_depth, top_k_max, ef_max, wal_frame_max } // 16 §8
pub struct Scoring { w_sim, w_recency, w_importance, w_access, w_confidence, half_life, c_norm, floor, time_axis, bias_routing }  // L4 排序打分,见 10
pub enum TimeAxis { ValidTime, TransactionTime }  // 新鲜度时间轴,见 10
pub enum Diversity { Off, Mmr { lambda: f32 } }                                     // 结果多样性,见 10
pub struct RelationKind(pub u16); // 关系类型:内置占用 0..=15(内置 0..=3),自定义从 16 起,见 09 §2.2
pub enum RelationIndex { Outgoing, Both }  // 关系反向索引,见 09
pub enum Feedback { Used, Ignored, Corrected { by: RowId } }  // 检索反馈,见 10
pub struct QueryId(pub u64);      // 一次检索的幂等标识(feedback 幂等键的一半),见 10 §4
pub struct UpdatePatch { /* 可选字段:vector/text/metadata/importance/ttl/valid_time/confidence/provenance;外层 None = 不改动 */ } // 见 03 §2.1
pub enum Compression { None, Lz4, Zstd }  // 文本/元数据压缩,feature(compress / compress-zstd),见 11

/// 时间源:TTL / 遗忘曲线 / touch 一律经此取"当前 Unix 毫秒"。
/// 生产用 SystemClock;测试注入可回拨/快进的假时钟,保证确定性。
pub trait Clock: Send + Sync { fn now_unix_ms(&self) -> i64; }
```

> `Encryption` 及其 `KeyProvider` / `Cipher` / `Key` 因依赖上层 trait 与 `encrypt`
> feature(且加密用的 `Key` 与 §1 的外部键 `Key` 同名),由 [11 安全层](11-security-storage.md)
> 定义,**不在 L0 的 options 模块中**;L0 只提供上表这些无上层依赖的数据定义。

`FsyncPolicy` 的语义与权衡见 [00 §6.1](00-fundamentals.md) 与
[04 §3](04-l2-persist.md);各配置项的默认值与 Builder 方法见
[16 §2](16-api-reference.md)。

---

## 9. 层边界契约(L0 → 上层)

L0 向上提供,且**只**提供:

1. 类型:`RowId / SlotId / SeqNo / SegmentId / NsId / Key / Meta / Dimension / FsyncPolicy /
   InsertMode / VectorFormat / HnswParams / CompactionPolicy / Tuning / Limits / Clock /
   SystemClock / Score / Scoring / TimeAxis / Diversity / RelationKind / RelationIndex /
   Feedback / QueryId / UpdatePatch / Compression`(除 `Score` 定义于 `metric.rs` 外,其余
   均为 options 模块中的数据定义,不含行为);
2. 数学:`Metric::{score, better, needs_norm}`、`simd::{dot, dot_scalar}`(后者为可移植
   标量参考实现,同时用于非 SIMD 架构回退)及其薄封装 `metric::{cosine, euclidean_sq}`
   (均归结为一次点积;两切片等长由调用方保证);
3. 容器:`TopK<T: Ord>`(同分按载荷升序,见 §5.1):`TopK::{new, push, merge, len,
   is_empty, capacity, into_sorted_vec}`;`BitSet`(存活/删除/块级位图共用);
   编解码:`varint::{encode_u32, encode_u64, decode_u32, decode_u64}`;
4. 文本:`text::tokenize`(按 Unicode 空白切词 + CJK bigram + 可选停用词,
   口径见 [06 §3.5](06-l4-query.md);契约 `FC-CORE-POST-008`);
5. 错误:`MnemeError` 与 `Result`。

**禁止**:任何 I/O、任何全局状态、任何锁、任何 `unsafe`(除 `simd/` 的 arch 内联)、
对 `serde` 的直接使用(只经 `meta.rs`)。所有函数必须是无 panic 的 `Result` 或
数学上可证明无 panic(切片长度由调用方断言)。

## 本章小结

- 六类标识符用 newtype:`RowId` 是稳定逻辑身份,`SlotId` 是段内物理槽位。
- 三种度量统一归结为**一次点积**;SIMD 手写三种内核 + 运行时分发,零依赖。
- `TopK` 是 $O(N \log k)$ 的有界堆(同分按载荷升序,需 `T: Ord`),支持并行 `merge`;varint 让小整数省空间。
- `meta.rs` 是唯一的 serde 隔离区;options 模块(按主题拆分的目录)集中全部配置类型与 `Clock`。
- **本章不变量**:I22(RowId 跨 update/upsert 稳定)。

## 下一章

[03-l1-memory.md](03-l1-memory.md):用这些积木搭出完整的公开 API 与全内存引擎。
