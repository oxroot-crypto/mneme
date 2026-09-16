# Mneme 与主流向量库性能对比

> 本目录是 Mneme 的横向基准工具与**手工整理**的结论报告。
> 自动运行产物（`results*/` 下的原始 JSON 与自动 Markdown）**不入版本控制**；
> 可追踪的数值以本文表格为准。工具使用方法见 §7。

## 1. 结论摘要

数据：50 000 行 × 128 维 f32、1 000 条查询、top-10、度量 L2²、M=16、
ef_construction=200、4 核构建、单线程查询（另测 4 线程总吞吐）。
主口径数据为「随机中心 + 簇内噪声」（近似真实嵌入分布），随机均匀数据作对照。
Mneme 同时给出默认 f32 与 `--quant i8`（两阶段精排）两条查询曲线。
**每个引擎在独立子进程中运行；多轮按「轮 → 引擎」轮转交错，报告取中位数，
并给出极差（正文表格为中位数）。**

| 维度 | 结论 |
| --- | --- |
| 召回质量 | **Mneme 与 hnsw_stable、usearch 同处第一梯队**（ef=128 时 0.9980 / 1.0000 / 0.9996，i8 档 0.9980），hnsw_rs 明显落后（0.9799）；instant-distance 0.9999（仅 ef=64）。Mneme ef=128 已达标设计门槛 Recall@10 ≥ 0.95 |
| 查询延迟 | usearch 最快（ef=128 时 P50 189 µs）；**Mneme i8 192.5 µs（与 usearch 基本持平）**，较默认 f32（295 µs）快约 35%；hnsw_stable 377 µs；hnsw_rs 441 µs；instant-distance 仅 ef=64 档 296 µs |
| 查询吞吐（4 线程） | usearch 17 020 QPS > **Mneme i8 13 699** > instant-distance 12 453 > Mneme f32 10 095 > hnsw_rs 5 599 ≈ hnsw_stable 5 502 |
| 构建速度 | usearch 3.8 s > hnsw_stable 8.7 s > **Mneme i8 10.5 s ≈ Mneme 10.9 s ≈ hnsw_rs 10.6 s** ≫ instant-distance 69.3 s。Mneme 是唯一走完整持久化路径（WAL + 段文件 + MANIFEST + 图落盘）的实现 |
| 内存（RSS 增量） | usearch 37.2 MiB ≈ hnsw_stable 39.4 MiB < instant-distance 67.4 MiB < **Mneme 97.3 MiB（i8 档 103.7 MiB）** < hnsw_rs 134.3 MiB |

一句话：**构建经批内并行改造（常驻 worker 池 + 批行数 128）后从 14.8 s 降到
10.9 s，与 hnsw_rs 打平、逼近 hnsw_stable（且含完整持久化）；查询默认 f32 档
与 hnsw_stable 持平，按需打开 i8 量化副本后 P50 149 µs（ef=64）/ 193 µs（ef=128），
与 usearch 基本持平，召回仅降 0.001 以内。i8 为按需选项（`--quant i8`），
不改变默认磁盘格式与结果口径。** 作为「存储引擎」形态（而非裸 ANN 库），
其核心取舍是可靠的持久化与记忆语义换来的固定开销。

## 2. 方法学

### 2.1 被测引擎

| 引擎 | 版本 | 形态 | 说明 |
| --- | --- | --- | --- |
| [Mneme](../README.md) | 0.1.0 | Rust，本地 path 依赖 | 持久化向量存储引擎；默认特性（mmap 开）、默认混合精度建图；L2²；`insert_batch` + `flush`；别名 `mneme_i8`（i8 量化副本）与 `mneme_mem`（纯内存库） |
| [usearch](https://crates.io/crates/usearch) | 2.26 | C++11 核心 + 官方 Rust 绑定 | 业界主流 SIMD ANN 库；L2sq；f32 |
| [hnsw_rs](https://crates.io/crates/hnsw_rs) | 0.3 | 纯 Rust | 开 `simdeez_f`（其推荐 SIMD 构建）；`parallel_insert` |
| [hnsw-stable](https://crates.io/crates/hnsw-stable) | 0.10 | 纯 Rust（hnswlib 移植） | 经典 C++ hnswlib 的 stable 分支纯 Rust 移植；L2 |
| [instant-distance](https://crates.io/crates/instant-distance) | 0.6 | 纯 Rust | InstantDomainSearch 生产实现；`M` 为库内常量 32 |

> 说明：未纳入 faiss / LanceDB / Qdrant——前者绑定需系统库且非进程内嵌入形态，
> 后两者属列存/服务形态，与「进程内嵌入」目标不可比。

### 2.2 数据

全部引擎共享同一份确定性数据（LCG 生成，固定 seed，无 `rand` 依赖）：

- **clustered（主口径）**：100 个随机中心 + 簇内均匀噪声（半幅 0.1），近似真实
  文本/图像嵌入的簇结构；
- **uniform（对照）**：逐分量均匀随机 `[-0.5, 0.5)`，即 ANN 最困难的一类输入
  （高维距离集中、近邻区分度低）；
- 真值：朴素 f32 暴力扫描（多线程分块）精确 top-10，逐查询计算 Recall@10。

### 2.3 参数与测量口径

- 图参数：`M=16`、`m0=32`（Mneme 默认即 2M）、`ef_construction=200`；
- 查询：`ef ∈ {64, 128, 256}`，`top_k=10`；
- 构建：并行度 4（Mneme 批内并行 / usearch 并行 `add` / hnsw_rs `parallel_insert` /
  hnsw-stable 并发 `insert`）；
- 查询：单线程逐条 `Instant` 计时，先预热 64 条；QPS 按均值折算；另测
  ef=128 下 4 线程总吞吐；
- **重复与可信度**：`--repeats N` 时编排器按「轮 → 引擎」轮转交错（同一时段
  共享机器状态），每引擎每轮开独立子进程；报告正文取各轮中位数，并输出
  「重复性」小节给出构建耗时、P50、Recall 与 4 线程吞吐的极差；全部原始轮次
  落盘 `raw.json` 备查。主口径 `--repeats 3`，uniform 对照 `--repeats 2`；
- Mneme 专属开关：`--quant f32|i8`（段量化副本；另有 `mneme_i8` 引擎别名，与默认
  f32 同轮交错）与 `mneme_mem`（纯内存库，无路径；`flush()` 建内存段后走 ANN）、
  `--build-precision f32|hybrid`
  与 `--hnsw-batch-rows`/`--hnsw-serial-rows`/`--hnsw-threads-max`/`--flush-threads`
  建图调参；其余引擎忽略这些参数（`--quant i8` 对应 `VectorFormat::I8Rescored`：
  i8 粗排 + f32 精排两阶段，建段抽样一致率低于门槛时该段自动回退 f32）；
- 内存：每引擎独立子进程，读 `/proc/self/status` 的 VmRSS，构建前后差值；
- 计时/内存均不受其他引擎污染（编排器串行 spawn 子进程）。

### 2.4 公平性与已知差异

- **Mneme 的构建耗时含 WAL、段文件、MANIFEST 与图落盘**（真实建库路径），
  其余竞品为纯内存建图——构建对比对 Mneme 略不利，这是刻意保留的口径；
- Mneme 查询走完整检索管线（段扫描、过滤位图、评分与融合框架），
  竞品为裸 HNSW 查询——这是「引擎 vs 索引库」的固有差异；
- instant-distance 的 `M` 为库内常量 32 且 `ef` 构建期锁定，只参与 ef=64 档；
- hnsw_rs 的 `parallel_insert` 使用 rayon 全局线程池（按核数），不受本工具
  线程参数控制；
- 索引内存列各引擎自报口径不同（分配器统计 vs 引擎估算），仅作量级参考。

## 3. 环境

| 项 | 值 |
| --- | --- |
| 主机 | VM-0-11-ubuntu |
| CPU | Intel(R) Xeon(R) Platinum 8255C @ 2.50 GHz，4 核 |
| 内存 | 15 GiB |
| 工具链 | rustc 1.97.1，`--release`（`lto = "thin"`，与主 crate 发布 profile 对齐） |
| 采集时间 | 2026-09-16（UTC），机器无其他负载任务；轮间极差见各表 |
| 重复口径 | clustered 3 轮 / uniform 2 轮，轮转交错取中位数；极差见各表 |

## 4. 结果

### 4.1 构建（clustered，50k×128，4 核；中位数［最小–最大］）

| 引擎 | 构建耗时 | 吞吐（行/s） | 索引内存(自报) | RSS 增量 |
| --- | ---: | ---: | ---: | ---: |
| mneme | 10.85 s［10.61–11.57］ | 4 607 | 24.4 MiB | 97.3 MiB |
| mneme (i8) | 10.52 s［10.43–11.44］ | 4 754 | 24.4 MiB | 103.7 MiB |
| usearch | 3.76 s［3.72–3.83］ | 13 296 | 80.4 MiB | 37.2 MiB |
| hnsw_rs | 10.61 s［10.59–11.36］ | 4 711 | — | 134.3 MiB |
| hnsw_stable | 8.66 s［8.09–8.89］ | 5 771 | — | 39.4 MiB |
| instant_distance | 69.32 s［69.02–69.65］ | 721 | — | 67.4 MiB |

> Mneme 构建分段：`insert_batch` 0.45 s + `flush` 10.29 s（f32；i8 同口径
> 0.45 s + 10.04 s）。`flush` 含 vsec/msec 编码、HNSW 建图与 hidx 落盘。
>
> 对照（uniform 困难集，2 轮中位数）：mneme 15.44 s、mneme (i8) 14.95 s、
> usearch 17.38 s、hnsw_rs 33.24 s、hnsw_stable 48.34 s、instant_distance 327.84 s
> ——均匀随机数据下 usearch 的建图更慢，Mneme 反超。

### 4.2 查询（clustered，单线程，1 000 条；中位数）

| 引擎 | ef | Recall@10 | P50 | P90 | P95 | P99 | QPS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| mneme | 64 | 0.9945 | 222.2 µs | 256.3 µs | 287.5 µs | 368.3 µs | 4 446 |
| mneme | 128 | 0.9980 | 295.1 µs | 340.4 µs | 369.6 µs | 489.5 µs | 3 312 |
| mneme | 256 | 1.0000 | 399.0 µs | 453.1 µs | 478.4 µs | 639.5 µs | 2 502 |
| mneme (i8) | 64 | 0.9938 | 149.3 µs | 172.0 µs | 187.6 µs | 231.8 µs | 6 564 |
| mneme (i8) | 128 | 0.9980 | 192.5 µs | 217.0 µs | 231.0 µs | 297.8 µs | 5 114 |
| mneme (i8) | 256 | 1.0000 | 275.1 µs | 303.2 µs | 320.2 µs | 407.5 µs | 3 625 |
| usearch | 64 | 0.9961 | 136.6 µs | 156.9 µs | 164.3 µs | 198.6 µs | 7 321 |
| usearch | 128 | 0.9996 | 188.9 µs | 214.8 µs | 226.9 µs | 267.9 µs | 5 264 |
| usearch | 256 | 0.9996 | 232.4 µs | 256.4 µs | 264.8 µs | 306.0 µs | 4 333 |
| hnsw_rs | 64 | 0.9657 | 301.5 µs | 414.4 µs | 462.1 µs | 599.9 µs | 3 087 |
| hnsw_rs | 128 | 0.9799 | 441.4 µs | 597.9 µs | 658.7 µs | 820.2 µs | 2 120 |
| hnsw_rs | 256 | 0.9861 | 711.5 µs | 859.1 µs | 938.0 µs | 1.16 ms | 1 374 |
| hnsw_stable | 64 | 0.9965 | 313.1 µs | 342.5 µs | 349.9 µs | 384.1 µs | 3 241 |
| hnsw_stable | 128 | 1.0000 | 377.2 µs | 412.9 µs | 425.4 µs | 541.9 µs | 2 676 |
| hnsw_stable | 256 | 1.0000 | 422.1 µs | 463.6 µs | 486.4 µs | 622.8 µs | 2 371 |
| instant_distance | 64 | 0.9999 | 296.1 µs | 321.4 µs | 330.0 µs | 340.9 µs | 3 415 |

4 线程总吞吐（ef=128；instant-distance 为 ef=64）：
usearch 17 020 ≥ Mneme (i8) 13 699 > instant-distance 12 453 > Mneme (f32) 10 095 >
hnsw_rs 5 599 ≈ hnsw_stable 5 502。

重复性示例（3 轮极差）：Mneme f32 ef=128 P50 290.8–306.7 µs、QPS 9 116–11 003；
usearch ef=128 P50 172.8–190.8 µs；Mneme 构建 10.61–11.57 s。全部原始轮次见
`raw.json`。

### 4.3 对照：随机均匀数据（ANN 困难集，2 轮中位数）

| 引擎 | ef=64 Recall / P50 | ef=128 Recall / P50 | ef=256 Recall / P50 |
| --- | ---: | ---: | ---: |
| mneme | 0.5137 / 554 µs | 0.6791 / 799 µs | 0.8210 / 1.30 ms |
| mneme (i8) | 0.5128 / 263 µs | 0.6783 / 426 µs | 0.8217 / 729 µs |
| usearch | 0.4477 / 318 µs | 0.6221 / 574 µs | 0.7916 / 1.08 ms |
| hnsw_rs | 0.5059 / 888 µs | 0.6654 / 1.39 ms | 0.8023 / 2.65 ms |
| hnsw_stable | 0.4466 / 827 µs | 0.6195 / 1.41 ms | 0.7867 / 2.70 ms |
| instant_distance | 0.7154 / 1.45 ms | — | — |

随机数据下**所有引擎召回一致塌到 0.45–0.82**：这不是某个实现的缺陷，而是
均匀高维随机向量的近邻区分度退化（距离集中）所致。Mneme 在每一档都是召回最高，
i8 档延迟反超 usearch（P50 263 µs vs 318 µs）；但该数据集不代表生产的嵌入分布，
故不作主口径。

### 4.4 Mneme 内存形态（`mneme_mem`，50k×128，3 轮中位数）

`mneme_mem` 为纯内存库（`Builder` 不设 `path`）在 `flush()` **建内存段**后的口径：
同一 `IndexFactory` 建 HNSW 图，但不写 vsec/msec/hidx、不做量化副本
（`FC-INDEX-INV-008`）。

| 口径 | 构建 | RSS 增量 | ef=128 P50 | ef=128 Recall | 4 线程 QPS |
| --- | ---: | ---: | ---: | ---: | ---: |
| mneme（持久） | 10.11 s | 96.1 MiB | 313.0 µs | 0.9980 | 10 142 |
| mneme_mem（纯内存） | 9.36 s | 80.0 MiB | 300.4 µs | 0.9980 | 10 955 |

省掉的是持久化开销（WAL / 段文件 / MANIFEST 的写盘与编码，构建约 −7%、RSS 约 −17%），
查询与召回和持久形态完全一致（段内是同一份 HNSW 图）。注意：**未调用 `flush()`
的纯内存库不做 ANN**，查询是精确暴力扫描（20k×128 实测 P50 ≈ 0.7 ms 且不随 `ef`
增长）；大规模内存库应先 `flush()` 建段。

## 5. 分析

1. **召回质量**：ef=128 时 mneme 0.9980、hnsw_stable 1.0000、usearch 0.9996，
   同处第一梯队；hnsw_rs 0.9799 落后 2 个百分点。
   Mneme 的建图选邻启发式（`hnsw_compare_cap=4` 等默认）在簇结构数据上表现稳健；
   按需打开 i8 量化副本后 ef=64 召回仅从 0.9945 降到 0.9938（两阶段精排保底），
   ef≥128 与 f32 档一致。
2. **查询延迟**：默认 f32 档与 hnsw_stable 持平（295 µs vs 377 µs @ ef=128），
   约为 usearch 的 1.56×；**i8 档把差距收敛到 1.02×**（192.5 µs vs 188.9 µs），
   ef=64 为 1.09×（149.3 µs vs 136.6 µs），且 P90/P99 分布整体前移——
   这是 Mneme 当前最有效的查询加速路径（i8 粗排把遍历距离的读带宽降到 1/4，
   f32 精排在候选集内重排保召回）。
3. **构建**：Mneme 10.85 s，与 hnsw_rs（10.61 s）打平、逼近 hnsw_stable（8.66 s），
   仍比 usearch 慢 2.9×（usearch 为纯内存零持久化）；在 uniform 困难集上
   Mneme（15.4 s）反超 usearch（17.4 s）。
   批内并行常驻 worker 池 + 默认批行数 128 的改造把 4 核实测构建从
   14.8–15.3 s 降到 10.5–10.9 s（−29%），召回不变；
   其中 `insert_batch`（WAL 追加）仅 0.45 s，其余是 flush 内的建图与编码。
4. **并发扩展**（4 线程总吞吐 ÷ 单线程 ef=128 QPS）：Mneme f32 3.05×、
   Mneme i8 2.68×、usearch 3.23×、hnsw_rs 2.64×、hnsw_stable 2.06×、
   instant-distance 3.65×（ef=64）。Mneme 扩展性正常，绝对吞吐受单线程基数限制。
5. **内存**：Mneme RSS 增量 97.3 MiB 高于 usearch/hnsw_stable（约 38 MiB），
   低于 hnsw_rs（134.3 MiB）；i8 档再 +6.4 MiB（码流 1 B/维）。
   注意 Mneme 子进程的 RSS 含 mmap 段页与数据集本身（所有引擎共同基线约 26 MiB），
   且自报口径（`memory_est`）只有 24.4 MiB。
6. **instant-distance** 是反面教材：构建单线程 69.3 s（比 usearch 慢 18×），
   但 M=32 与树状构建换来不错的查询表现——说明本对比中构建与查询的取舍空间很大。

## 6. 局限与后续

- 4 核共享 VM 上仍会有 ±3–8% 的轮间波动（本报告已给出中位数与极差、
  原始轮次落盘 `raw.json` 备查）；结论适合量级比较，不宜当精确排名；
- i8 量化副本（`VectorFormat::I8Rescored`，`--quant i8` / `mneme_i8`）作为
  **按需加速选项**推荐：P50 约降 35%、4 线程吞吐 +36%（10 095 → 13 699），
  召回降幅 ≤0.001；**不改为默认**，以免改变磁盘格式与默认结果口径；
- 未测过滤查询、BM25 混合检索、压缩/加密等存储能力——竞品无对应能力，不可比；
- 未测冷启动（mmap 惰性加载）与崩溃恢复——Mneme 的差异化能力，需专项基准；
- `instant-distance` 的 `ef` 构建期锁定导致只参与一档；`M=32` 与其他引擎的
  M=16 不同口径，阅读其数字时需注意。

## 7. 复现

```bash
# 在 comparison/ 目录（独立 workspace，依赖与主 crate 隔离）
cd comparison

# 主口径：簇结构数据，6 引擎（含 mneme 与 mneme_i8）轮转交错 × 3 轮
cargo run --release -- run --data-mode clustered \
  --rows 50000 --dim 128 --queries 1000 --k 10 \
  --ef 64,128,256 --threads 4 --qthreads 4 --repeats 3 --out results-clustered

# 对照：均匀随机数据（instant_distance 建库约 330 s/轮，故 2 轮）
cargo run --release -- run --data-mode uniform \
  --rows 50000 --dim 128 --queries 1000 --k 10 \
  --ef 64,128,256 --threads 4 --qthreads 4 --repeats 2 --out results

# 单引擎调试（直接输出 @@RESULT@@<json>）
cargo run --release -- run-one --engine mneme --rows 5000 --dim 64 --queries 200

# 建图调参 A/B（仅 Mneme 消费）
cargo run --release -- run-one --engine mneme --rows 50000 --dim 128 \
  --queries 1000 --k 10 --ef 64 --threads 4 --build-precision f32 \
  --hnsw-batch-rows 128 --hnsw-threads-max 8
```

自动产物（`results*/RESULTS.md`、`run-*.json`、`latest.json`、`raw.json`）已在
`.gitignore` 中排除；如需留档请手工整理进本文件的表格。

## 8. 规范与豁免

本目录是**开发期基准工具**，不承载引擎业务逻辑、无对外契约、不进入发布产物
（独立 workspace，依赖不污染主 crate 的 `[dependencies]`/`[dev-dependencies]`
与 CI）。按 FSVDD 规范 §1.2 属豁免面：不登记 `FC-*`、不参与契约追溯门禁；
工具自身的参数校验与错误传播仍按 `rules/rust.md` 的标准实现（附单元测试）。
