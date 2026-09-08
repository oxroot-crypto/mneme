# 07 L5 生命周期层:遗忘与不失控

> **本章目标**:兑现"超长期"承诺——TTL 自动过期、艾宾浩斯式遗忘、
> size-tiered compaction 保证段数有界,以及快照/备份/统计等运维面。
> **前置阅读**:[04](04-l2-persist.md)(段与 Manifest)、[05 §7](05-l3-hnsw.md)(墓碑与图重建)、[00 §6.5/§6.7](00-fundamentals.md)(compaction/写放大科普)。
> **本章你将学到**:TTL 双阶段过期 → 访问统计的零写放大设计 → 指数遗忘曲线推导 →
> compaction 写放大分析 → 命名空间 → 快照备份 → stats/fsck。

模块:`life/{ttl.rs, retain.rs, access.rs, namespace.rs, compact.rs, backup.rs, stats.rs}`

---

## 1. TTL:两阶段过期

**TTL(Time-To-Live,存活期)**:记录自带 `expires_at = created_at + ttl`。
过期采用两阶段,兼顾正确延迟与成本:

| 阶段 | 时机 | 机制 | 成本 |
|---|---|---|---|
| ① 逻辑过期 | 查询/统计时 | 命中即判 `now ≥ expires_at` → 视为不存在;段内每块存 `min(expires_at)`(msec `ttl_map`,[04 §2.2](04-l2-persist.md)),**整块全未过期时零逐行判断** | O(块数) |
| ② 物理清除 | compaction 时 | 过期行不写入新段,空间真正回收 | 摊销进 compaction |

不变量:**过期只可能"晚消失",不可能"早消失"**——逻辑过期兜住正确性,
物理清除只负责回收。`ns.forget(filter)` 是人工 TTL:立即打墓碑(逻辑过期),
等 compaction 物理回收。

`now` 一律取自可注入的 `Clock`([04 §10.2](04-l2-persist.md)),并做单调钳制,
因此测试可"快进时间"、系统时钟回拨也不会导致误过期。

---

## 2. 访问统计:`access.rs`(遗忘曲线的数据地基)

"被想起的记忆更该被记住"——需要每条记忆的 `last_access` 与 `access_count`。
**难点**:读路径若为更新统计而写盘,读就有写放大,违背"读多写少"的负载假设。

**设计:内存累积 + 批量落盘**:

```text
查询命中 → 读者把 RowId 追加进线程本地缓冲 → 攒批后合并进
  Mutex<HashMap<RowId, AccessStat>>     (内存,写者锁外;AccessStat 见 [11 §1.6](11-api-reference.md))
后台每 30s(默认): 把增量以 WAL Touch 帧落盘(一帧合并多次命中)
compaction: 把 Touch 历史并入新 msec 的 last_access / access_count 列([04 §2.2](04-l2-persist.md) entry 格式)
```

- 读路径热区只有一次内存 `push`,**写放大 = 0**;
- 崩溃最多丢 30s 的访问计数——它只影响遗忘速度的估计,不影响正确性,可接受;
- `ns.touch(key, boost)` 是显式强化:立即 WAL Touch 帧,`boost = Some(d)` 时附带 `importance` 提升
  (Agent 明确说"这点很重要"时)。

---

## 3. 遗忘曲线:`retain.rs`

### 3.1 【直觉】人脑怎么忘

艾宾浩斯(Ebbinghaus, 1885)的记忆实验揭示:记忆保持率随时间**指数衰减**,
且**每次回忆都会显著减缓衰减**。 Agent 记忆应当一样:临时搜索结果一周就淡忘,
核心偏好经久不忘,常被引用的记忆最顽固。把这条曲线做成引擎策略,而不是应用层
定时任务——因为引擎手里有 compaction 这个"定时大扫除",顺手且一致。

### 3.2 【数学】指数衰减与半衰期

保留强度(how strong a memory still is):

$$E(t) = I \cdot 2^{-t/T_{1/2}}$$

(纯文本:`E(t) = I * 2^(-t / T_half)`,I = 初始 importance ∈ [0,1],t = 距上次强化的时长)

等价指数形式 $E(t) = I \cdot e^{-\lambda t}$,$\lambda = \ln 2 / T_{1/2}$。

**为什么用指数(而非线性/阶跃)?** 核心性质——**任意等长时间内衰减相同比例**:

$$\frac{E(t + T_{1/2})}{E(t)} = \frac{I \cdot 2^{-(t+T_{1/2})/T_{1/2}}}{I \cdot 2^{-t/T_{1/2}}} = 2^{-1} = \frac{1}{2}$$

(纯文本:每过一个半衰期,强度减半,与当前强度无关)

这与"遗忘没有终点、只有渐近"的心理现象一致;线性衰减会归零(记忆"死透"),
阶跃衰减没有"渐淡"过程。半衰期参数 $T_{1/2}$ 比速率 $\lambda$ 直观:
"两周忘一半"人人能懂。

### 3.3 【数学】访问增益:对数的边际递减

被回忆会刷新 $t$(重置时钟)并计入 $c$(累计次数)。有效强度:

$$S = I \cdot 2^{-t/T_{1/2}} + w \cdot \ln(1 + c)$$

(纯文本:`S = I * 2^(-t / T_half) + w * ln(1 + c)`,c = 累计访问次数)

**为什么增益是 $\ln(1+c)$?** 其导数 $\frac{d}{dc}\ln(1+c) = \frac{1}{1+c}$
单调递减——第 1 次回忆的强化远大于第 100 次(边际递减),符合直觉;
且 $\ln$ 增长极慢,任何记忆都无法靠刷访问次数变成"不朽"
($c = 10^6$ 也只加 $w \times 13.8$),$w$ 默认 0.05。

### 3.4 Retain 算法与算例

```text
ns.retain(policy):  扫描候选(过滤 + protect 白名单豁免)
  S = I·2^(-t/T½) + w·ln(1+c)
  S < min_importance → 打墓碑(逻辑过期,物理回收留给 compaction)
   返回 (扫描数, 遗忘数);也可由后台周期性自动执行
   (默认开,周期 = 半衰期/4,可用 `Builder::retain_interval` 调整;默认策略经
    `retention(Option<Retention>)` 配置,`retention(None)` 关闭自动模式,
    见 [11 §2](11-api-reference.md))
```

**【算例】** $T_{1/2} = 14$ 天,$w = 0.05$,$min\_importance = 0.2$:

```
记忆 A: I=0.8, 从未访问, 28 天未强化 → S = 0.8 × 2^(-28/14) = 0.8 × 0.25 = 0.20
        → 恰在阈值边缘(≥0.2 保留)
记忆 B: I=0.8, 同龄, 但被访问过 3 次 → S = 0.20 + 0.05·ln(4) ≈ 0.20 + 0.069 = 0.269 → 保留 ✓
记忆 C: I=0.2 的临时记录, 14 天未动 → S = 0.2 × 0.5 = 0.10 → 遗忘 ✓
```

**【复杂度】** 一次 retain 扫描 $O(N_{\text{候选}})$(元数据级,不读向量);
自动模式摊销进后台,周期 = 半衰期/4(默认),单次成本与 compaction 同量级、受同一限速。

---

## 4. Compaction:`compact.rs`——超长期的定海神针

### 4.1 【直觉】整理书桌

追加写让桌面(段文件)越堆越多:旧草稿(墓碑)、过期的便签(TTL)混在里面。
定期把几摞纸**誊清成一张新纸**(有效记录重写到新段),桌面重新整洁,
誊写时顺手:物理删除墓碑、清除 TTL 过期、执行 retain 评分、重建索引
(向量图 + 倒排)。

### 4.2 【数学】size-tiered 分级合并

**规则**:段按大小分层(层 $i$ 的大小 $\approx B \cdot r^i$,$B$ = 段初始行数,
$r$ = 分级比,默认 4;每行大小近似常数,故行数正比于字节数);
**同层攒够 $T$ 个(默认 4)即合并**成一个上一层的段。

**写放大推导**(每字节平均被重写次数):数据从层 0 出发,每被合并一次升一层,
到顶层 $L = \log_r(N/B)$ 层,之后不再动。设 $T = r$(默认均为 4):
层 $i$ 单次合并重写约 $r \cdot B r^{i}$ 行($r$ 个大小 $B r^i$ 的段),
该层合并次数约 $N / (r \cdot B r^{i})$——**每层总写入都约等于 $N$ 行**;
共 $L$ 层,故总写入 $\approx N \cdot L$,即每字节期望重写次数:

$$W_{\text{amp}} \;\approx\; \frac{\text{全层总写入}}{N} \;\approx\; L \;=\; \log_r\frac{N}{B}$$

(几何级数各项——每层的总写入——近似相等,是 size-tiered 写放大可控的关键;
保守上界带常数因子 $\frac{r}{r-1}$。)
代入 $r=4$:$N = 10^8$ 行(远超实际),$B$ = 8k 行 → $\log_4 12500 \approx 6.8$,
即**平均每字节被重写约 7 次**(上界 ≈ 9,仍是个位数)——写放大可预测、可接受。

**段数有界证明**:层 $i$ 的段数 ≤ $T-1$(否则触发合并),层数 $\log_r(N/B)$
→ 活跃段数 $\le (T-1)\log_r(N/B) + O(1) = O(\log_r N)$。这就是
[01 §1.1](01-overview.md)"超长期不失控"的数学根据:**无论跑十年还是五十年,
打开的文件数只随数据量对数增长**。

### 4.3 触发条件(任一满足)

| 条件 | 默认阈值 | 针对的问题 |
|---|---|---|
| 同层段数 | ≥ 4 | 段数蔓延 |
| 墓碑+过期占比 | > 25% | 空间/召回浪费(死节点穿越成本,见 [05 §7](05-l3-hnsw.md)) |
| WAL 压力 | WAL > 256MB | flush 频率过高(小段过多) |

### 4.4 流程(与崩溃安全)

```text
1. 调度线程选段组(最少写入热度的优先)→ 生成合并计划(登记,可取消)
2. scoped threads 并行: 逐行过滤(墓碑/TTL/retain 评分)→ 写新 vsec/msec
3. 对幸存行并行重建 HNSW(05 §4 的 build,分块并行)+ 重建倒排/zone map
4. 提交: 新段写完 + CRC → 新 MANIFEST(原子,04 §6)→ 旧段进 trash(04 §9)
失败: 任意一步崩溃 → 新段是孤儿(下轮启动清理), 旧 MANIFEST 完好, 无损回滚
```

- **限速**:合并 IO 与前台共享配额(默认磁盘预算 30%),写竞争时主动让路
  (`db.compact_control()` 的 `pause()` / `resume()`,见 [11 §1.6](11-api-reference.md)),保证查询 P99 不被 compaction 拖爆;
- **查询可见性**:合并期间新旧段同时在 MANIFEST 里吗?不——旧段保持到提交瞬间,
  新段在提交后可见,中间的读者要么看旧要么看新(快照语义),永不看到半成品;
- **结果稳定性**:重建会重排 HNSW 邻接(并行构建 + 新段布局),故同一数据在合并前后
  的近似检索结果可能不同——这是 [05 §6.2](05-l3-hnsw.md) 明示的确定性边界,
  快照句柄(I17)保证的只是"钉住的视图内不变",不保证跨 compaction 相同。

### 4.5 【复杂度】

| 操作 | 时间 | 说明 |
|---|---|---|
| 单轮合并(数据量 $S$) | $O(S \cdot d \cdot ef_c \cdot M_0)$ | 建图主导(复杂度见 [05 §6.1](05-l3-hnsw.md));逐行过滤/重写/倒排与 zone map 重建均为线性 |
| 摊还到每条写入 | $O(d \cdot ef_c \cdot M_0 \cdot W_{\text{amp}})$ | $W_{\text{amp}}$ ≈ 7(§4.2) |
| 空间峰值 | 额外 $O(S)$ | 新段 + trash,提交后回落 |

---

## 5. 命名空间:`namespace.rs`

- 路径式层级:`db.namespace("agent-42/session-88")`,内部是记录上的隐式字段
  `__ns`(不占用户元数据,`created_at` 式的常驻索引);
- **键唯一性按命名空间隔离**:`(NsId, Key)` 才是主键——两个 Agent 可以都用 `"mem_1"`;
- **NsId 分配**:单调递增的 `u32`([02 §1](02-l0-core.md)),由 MANIFEST 的**命名空间
  注册表**持久化(`NsEntry { ns_id, path }` 列表 + `next_ns_id` 水位,[04 §2.4](04-l2-persist.md));
  **永不复用**——命名空间删除后其 NsId 作废,新命名空间拿新号,避免旧引用指向新空间;
  `list_namespaces()` 直接读该注册表(路径 ↔ NsId),重启后依然可枚举;
- 物理形态:**单库前缀聚合**(同库内所有 NS 共享段,按前缀过滤)。
  依据:段内 zone map 对 `__ns` 等值极高效(通常整块剪除),独立目录的强隔离收益
  (单独备份/删除某 Agent)暂无场景;未来若需要,升级路径是"NS → 子目录",
  接口(`namespace()` 语义)不变;
- **生命周期 API**:

```rust
db.namespace("a/b");            // 不存在则隐式创建(空命名空间不占物理空间)
db.list_namespaces()?;          // 按前缀树顺序列出全部路径
db.drop_namespace("a/b")?;      // 墓碑该前缀下所有记录(含子命名空间),返回行数;
                                // 物理回收留给 compaction
ns.iter(None)?;                 // 遍历/导出一个命名空间的全部活记录
```

- **注册时机**:`namespace(path)` 只返回一个轻量句柄(无 `Result`、不写盘);
  `NsEntry` 在该命名空间**首次成功写入**(`insert`/`insert_batch`,经 WAL 提交)时
  惰性登记进 MANIFEST。因此 `list_namespaces()` 列的是"注册过的"命名空间——
  仅调用过 `namespace()` 但从未写入的空空间不会出现(它也不占物理空间);
- per-NS 统计:来自 msec 的 `ns_stats`(每段每命名空间的 `doc_count`/`total_doc_len`,
  [04 §5.6](04-l2-persist.md))聚合(`db.stats()` 的 `per_namespace`)。

---

## 6. 快照与备份:`backup.rs`

```text
db.snapshot()  → SnapshotHandle: 钉住当前 ReaderView(该时刻的 MANIFEST 段集 +
                 可变表不可变快照 + 各段 Arc 引用)
                 (即使后台合并推进, 快照视图依然完整可查 —— 时间旅行读;
                  含取快照前所有已确认写入, 无需先 flush, 见 04 §8)
db.backup_to(dir) → 先 flush() 形成一致性点, 再在快照视图上:
                    硬链接各段文件(失败/跨盘时回退复制)
                 → 复制 current 与 MANIFEST.<v> → 目标目录即可被 Mneme::open 独立打开
```

`SnapshotHandle` 的完整接口(`version / search / get / get_by_rowid / get_vector / iter / stats`)
见 [11 §1.6](11-api-reference.md);它 `Send + Sync`,可交给只读线程做长查询而
不阻塞前台写入。

- **一致性**:硬链接的文件集来自**同一 Manifest 版本**,而段文件 write-once——
  备份期间的前台写入不污染备份(它们写的是新段);
- 成本:同盘硬链接 $O(\text{文件数})$;跨盘复制 = 数据量,可在 `stats()` 里预估;
- 恢复演练:备份目录 `open` + `check()` 全绿 = 备份有效(写进 CI 的验收项,
  见 [09 §6](09-testing.md));
- **时间点恢复 / 损坏处置**:MANIFEST 保留最近 2 个版本,把 `current` 指向上一个
  版本号即可回滚一个提交点;完整 runbook(含 ENOSPC、MANIFEST 全坏、段隔离)
  见 [11 §7](11-api-reference.md)。

---

## 7. stats 与 fsck:`stats.rs`

```rust
db.stats()?  -> Stats {
    segments: Vec<SegmentStat{ id, rows, bytes, dead_ratio, created }>,
    wal_bytes, memory_est, trash_bytes,
    query_latency: Histogram(固定桶: 1ms..1s, 32 桶),
    per_namespace: HashMap<String, NsStat>,   // 键为命名空间路径(经 MANIFEST 注册表解析)
    compaction: CompactionState,   // Idle | Running{progress, segments},定义见 [11 §1.6](11-api-reference.md)
}
db.check()?   // fsck: 全量 CRC + slot 表/RowId 映射一致性 + key 索引 ↔ entries 对账
              //           + 墓碑/TTL 占比报告 + 建议动作(如 "建议合并 3 个 25MB 段")
```

直方图用固定 32 桶(对数刻度)——不引 HDR histogram 依赖,精度对运维够用。

---

## 8. 层边界契约(L5 → 上层)

**向上提供**:`retain / forget / touch / ttl` 全套语义([01 §6](01-overview.md) API)、
命名空间隔离与生命周期(`namespace / list_namespaces / drop_namespace / iter`)、
`snapshot → SnapshotHandle` / `backup_to / stats / check`、后台 compaction 调度
(可配可暂停)。

**依赖**:L2(段/Manifest/WAL/trash)、L3(`HnswIndex::build` 重建)、L4(计划器供 retain 扫描)。

**不变量**:

- I8 任意时刻活跃段数 ≤ $(T-1)\cdot\log_r(N/B) + c$;WAL 总量 ≤ 256MB(默认);
- I9 逻辑过期/墓碑记录永不返回给读者;物理回收只发生在 compaction 提交点之后;
- I10 compaction 任意时刻崩溃 → 恢复后数据集 = 提交前状态(孤儿段自动清理);
- I11 备份目录独立打开 + check 通过;
- I17 `SnapshotHandle` 存活期间看到固定 ReaderView 的完整视图(段集 + 取快照时的可变表快照),后台 compaction 不影响其正确性。

## 下一章

[08-l6-quant.md](08-l6-quant.md):量化与两阶段检索,把查询带宽砍掉四分之三。
