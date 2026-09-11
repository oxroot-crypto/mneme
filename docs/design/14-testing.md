# 14 测试与验收:如何证明以上不是纸上谈兵

> **本章目标**:把各章与 [16](16-api-reference.md) 的每个"不变量(I1–I30)"和"性能承诺"
> 翻译成可执行的测试。
> **前置阅读**:各章"层边界契约"小节。
> **本章你将学到**:测试金字塔 → 不变量映射表 → 崩溃注入方法论 → 召回属性测试 →
> 基准目标 → fuzz → 长跑。

---

## 1. 测试金字塔

```text
        ┌─────────────┐
        │  长跑/基准   │   24h 长跑、criterion 性能门槛(夜间)
        ├─────────────┤
        │  集成测试    │   分层验收:L1 API / L2 崩溃 / L3 召回 / L4 混合 / L5 闭环
        ├─────────────┤
        │  属性测试    │   proptest:varint 往返、DSL roundtrip、崩溃前缀不变量
        ├─────────────┤
        │  单元测试    │   每模块;距离函数 vs 标量参考、TopK、varint、解析器
        └─────────────┘
```

**总纲**:每章的"不变量"编号(I1–I30,见各章末尾与 [16 §9](16-api-reference.md))
就是测试断言的锚点——测试代码中的注释必须引用不变量编号,防止"测了个寂寞"。

### 1.1 不变量 → 测试映射

| 不变量 | 含义 | 验收位置 |
|---|---|---|
| I1–I4 | 持久化/损坏检测/段集合/WAL 有界 | §2 崩溃注入 |
| I5 | 混合检索等价性 | §3 召回属性测试 |
| I6 | 过滤先行,与融合顺序无关 | §3.1 融合顺序交换测试 |
| I7 | DSL 任意输入不 panic | §5 `fuzz_dsl` |
| I8–I11 | 段数有界 / 过期零返回 / compaction 原子 / 备份可独立打开 | §6 长跑测试 |
| I12 | 量化模式 `Hit.score` 为 f32 精排分 | §3.2 分数口径断言(基准见 §4) |
| I13 | 量化召回不达标自动回退 f32 | §3.2 自动回退测试 |
| I14 | async 与 sync API 等价 | §3.3 等价性测试 |
| I15 | `insert_batch` 整批原子 | §2.1 批量崩溃注入 |
| I16 | `close()` 后已确认写入持久 | §2.2 关闭持久性测试 |
| I17 | `SnapshotHandle` 视图一致 | §6.1 快照并发测试 |
| I18 | 拒绝打开更高格式版本 | §5 `fuzz_vsec` + 版本注入测试 |
| I19 | 覆盖持久性(删除不复活/更新不丢) | §2.4 覆盖崩溃注入 |
| I20 | 注册与水位可恢复 | §2.5 注册恢复测试 |
| I21 | BM25 统计一致性(跨段全局 + NS 隔离) | §3.4 跨段 BM25 测试 |
| I22 | RowId 跨更新稳定 | §2.3 稳定 RowId 测试 |
| I23 | 删除可审计与安全默认 | §6.2 遗忘审计测试 |
| I24 | 更新原子可见 | §3.5 更新可见性测试 |
| I25 | 关系一致性 | §3.6 关系级联测试 |
| I26 | 双时态一致(历史默认永久保留,受 `history_horizon` 约束) | §6.3 `as_of` 历史读测试 |
| I27 | 反馈幂等 | §3.7 反馈幂等测试 |
| I28 | 加密不落明文 | §5.3 加密/篡改测试 |
| I29 | 只读一致 | §6.4 只读共享测试 |
| I30 | 可观测无副作用 | §6.5 Observer 隔离测试 |

**另**:L1 的"排序全等性"([03 §2.2](03-l1-memory.md))已登记为
`FC-INDEX-POST-003`;L3 的"`ef → ∞` 收敛"([05 §12](05-l3-hnsw.md))对应
`FC-INDEX-POST-002`;版本链历史保留对应 `FC-MODEL-POST-004`。均作为属性测试断言。
写入期去重语义(`FC-INDEX-POST-004`)与入边方向(`FC-MODEL-POST-005`)分别在 §2.3/§3.6 验收。
**契约追溯**:每条不变量在 [spec/contracts.md](../spec/contracts.md) 至少对应一条 `FC-*`
条目(矩阵另含非不变量类约束,如 POST/ERR/STA);CI 校验无孤儿/无遗漏。

---

## 2. 崩溃注入(I1–I4,I15,I16 的验收)

> **L2 落地状态**:基于 `FsyncHook` 的确定性崩溃/撕裂写测试已实现并通过
> (`tests/persist_contracts.rs`);下述"1000 次 × 10k 操作"的 **proptest 崩溃前缀 harness**
> 与 CI 每夜抽样**尚未接入**,是后续落地目标(见 §7)。

**方法论**:`FsyncHook`([04 §10](04-l2-persist.md))在每次 write/fsync/rename 前被调用,
测试注入三种故障:

| 故障 | 注入方式 | 断言 |
|---|---|---|
| 随机点"断电" | 第 N 次操作后进程 `std::process::abort()` | 重开后状态 = 已确认操作前缀 |
| 撕裂写 | 把最后一帧/最后一块写一半(截断字节流) | CRC 拦截;恢复不报损坏(正常丢弃) |
| 位翻转 | 随机翻转数据区/头部 1 bit | 校验层检出:`Corrupted` 或段隔离,绝不静默错数据 |

**前缀不变量**(核心断言,proptest 驱动):

```text
对任意操作序列 ops = [o1..on], 在任意 oi 之后的崩溃点:
  恢复后的可见状态 ≡ repl(ops[1..j]), 其中 j ≥ 最后一个"返回 Ok"的操作下标
  (可见 = 前缀且不少于已确认部分;Batched 策略允许 j 超前已确认者,
   因为 OS 可能已刷盘——重放幂等,多不可错)
```

规模:1000 次随机崩溃 × 10k 操作序列,CI 每夜全跑,提交时抽样 50 组。

### 2.1 批量原子性(I15)

```text
对随机批大小 n∈[1,1000],在 insert_batch 的 WAL 追加/fsync/BatchCommit 任意步骤崩溃:
  恢复后可见记录数 ∈ {0, n}(整批),绝不出现 0 < m < n 的部分批
```

### 2.2 关闭持久性(I16)

```text
写若干记录 → db.close() 返回 Ok → 重开:
  所有返回过 Ok 的写入均可见
对照:不调用 close 直接 drop,允许丢失未 flush 的写入,但绝不出现半写记录(I1)
```

### 2.3 身份与索引对账(RowId / key / 命名空间)

```text
RowId 稳定: 记录写入 → 触发多轮 compaction → 同一 RowId 仍指向同一逻辑记录,
            且 get_by_rowid(id) 与 get(key) 返回同一行
key 索引:   随机 key 集写入 → 重启 → 每个存活 key 的 get(key) 均命中且内容一致;
            不存在的 key 经 bloom 快速否定(不返回错数据)
跨段覆盖:   同 key 连续 upsert 使其分布在多个未合并段 → get(key) 恒返回 seqno 最大者;
            delete(key) 后所有旧版本不可见;compaction 后仍一致(04 §5.5)
命名空间:   建多个嵌套 NS 并写入 → 重启 → list_namespaces() 与写入前完全一致;
            drop_namespace 后其路径从注册表移除, NsId 不再被新空间复用
非法输入:   含 NaN/Inf 的向量或 NaN importance/confidence insert → NonFinite;
            策略参数 NaN(dedup_threshold/min_importance/access_weight/threshold、max_cluster=0)→ Config;
            MMR lambda 非有限值、dedup_threshold/threshold 越界 [0,1] → Config;
            insert_batch 中 Merge 回调产物超限 → 整批回滚、零部分写入(FC-MEM-POST-002);
            update 超限 patch → TooLarge/MetaTooDeep 且保持原版本;库内数据不被污染(03 §2.1)
文件锁:     活实例持有 → Busy;持锁进程死亡(OS 咨询锁自动释放)→ 再次 open 成功而非永久 Busy(16 §3)
稳定RowId:  同 key 连续 update/upsert 多轮 → RowId 始终不变(或按语义稳定),
            访问统计与关系边仍指向同一逻辑记忆(I22)
去重语义:   Dedup::Merge 就地更新并返回 Merged(old)、保留旧 RowId;Dedup::Replace 生成新 RowId
            并墓碑旧行;insert_batch 中 RejectDuplicate/Dedup::Reject 逐条返回 Duplicate,
            其余记录照常写入(不整批回滚)
```

### 2.4 覆盖持久性(I19)

```text
对"记录在旧段"的场景:
  写记录 → flush 成段 → delete(key) / update(key,patch) / touch(key,boost) →
  在 WAL 重置之前/之后任意点崩溃(L2:全量快照 flush 提交 MANIFEST 后再重置 WAL) → 重开:
    删除的记录永不复活;update 的字段与 touch 的 importance 提升仍生效
  再触发多轮 compaction 后重复断言(L5)
反例保护:在覆盖条目物化(随快照段落盘)前截断 WAL → 必须拒绝(FC-PERSIST-POST-002)
```

### 2.5 注册与水位恢复(I20)

```text
新建命名空间首次写入 → 不 flush 直接崩溃 → 重开:
  list_namespaces() 含该路径;向同一路径再写入仍映射到同一 NsId;
  NsId 水位不回退,旧 NsId 不被新空间复用
RowId 水位:回放含大 rowid 的 WAL → next_rowid > 该 rowid
```

---

## 3. 召回与等价性属性测试(I5,I6,I12,I13,I14 的验收;L3/L4)

> **L3 落地状态**:召回 / `ef→∞` 收敛 / 过滤三档 / hidx 往返与损坏 / 重开载入(含
> 非恒等重排映射)与 `as_of` + ANN 已在 `tests/hnsw_contracts.rs` 与
> `src/index/{hnsw,hidx}.rs` 单测中实现并通过;hidx 另以 proptest 断言任意字节不 panic。
>
> **L4 落地状态(2026-09)**:单通道/带过滤候选的向量暴力对照、过滤先行(含 `top_k` 小于
> 候选数的证伪构造)、融合参数校验与精确分数、BM25 精确分数/NS 隔离/只计活行、四区落盘
> 重开一致性(分数逐位)、计划器不漏报(大整数/类型污染/非数值 `exists`)与历史视图 TTL,
> 已在 `tests/l4_contracts.rs` 与 `src/query/*` 单测落地并纳入追溯门禁。**欠账**:§3 的
> "逐行过滤 + 双通道暴力 + 融合"全流程 oracle 与 §3.4 的"跨 3 个未合并段"依赖多段形态,
> 待 L5 段句柄/多段落地后补齐(L4 当前为单段 + 尾部增量);`fuzz_dsl`(cargo-fuzz)仍待接线(§5)。

```text
数据: 种子固定;随机均匀 64 维 10 万条 + 8 簇合成数据 10 万条(两套)
      **L3 当前 CI 档**:2500 条 × 32 维的两套微缩数据(默认 HnswParams,
      簇状档为"8 质心 + 确定性小扰动"的近似高斯);
      设计规模(10 万 × 64 维)属 heavy/夜间档,尚未接线
参照: 每查询在候选集上暴力精确计算 → 标准答案
断言: Recall@10(HNSW, ef=128) ≥ 0.95(两套数据分别达标)
      过滤档③结果 ≡ "候选位图内暴力"(集合相等);档①②与之统计等价(多查询召回门槛)
      ef → ∞ 时 HNSW 结果 → 精确(收敛性抽测)
      排序全等性: 同一快照内同参数重复 execute() 结果逐位相同(FC-INDEX-POST-003)
```

混合检索(L4)的等价参照:同一 DSL 下"逐行过滤 + 双通道暴力 + 融合"作为 oracle。
BM25 统计范围:同段内混入两个命名空间的文档,断言对命名空间 A 的查询结果
**不受**命名空间 B 文档数量影响(对比"段级统计"会产生的排序漂移,04 §5.6)。

### 3.1 过滤与融合顺序(I6)

构造"低重要度记录在两通道都排名更高"且 `top_k` 小于候选数的数据,执行
"过滤 → 双通道 → 融合":断言结果集严格等于过滤后候选且全部满足过滤。
若实现"先融合截断再过滤",该构造下结果会变少甚至为空,测试必然失败(见
`tests/l4_contracts.rs::filter_is_order_independent`)。

### 3.2 量化分数口径与自动回退(I12,I13)

```text
I12: 量化模式下每个 Hit.score == 对同一候选用 f32 重算的分数(逐位/容差内相等)
I13: 注入一个"召回劣化"的假量化器 → 建库基准不达标 → 断言自动回退 f32,
     且 stats() 的量化状态显示已回退
```

### 3.3 async 与 sync 等价(I14)

```text
对同一操作序列,分别在同步 API 与 feature="async" 下执行:
  最终状态、每步返回值、错误类型逐一相等(共享同一把写锁)
```

### 3.4 BM25 全局统计(I21)

```text
构造同一命名空间跨 3 个未合并段的数据 → 查询词横跨多段:
  断言 score 与"把三段合并成一段后的 score"在容差内一致(段数无关)
死行不计:对旧版本 upsert 后,df_t 不包含被遮蔽版本;与"物理删除后"结果一致
命名空间隔离:向 NS A 混入大量 NS B 文档 → A 的排序不变(04 §5.6)
```

### 3.5 更新原子可见(I24)

```text
update(key, patch) 与并发查询交错:任一查询要么看到旧版本、要么看到新版本,
绝不看到字段混合的半更新;更新后旧版本立即不可见
```

### 3.6 关系级联(I25)

```text
建立 A→B、B→C 边 → 删除 B → neighbors(A) 不含 B,且不返回悬挂边;
as_of(删除前) 在 compaction 回收该版本前仍能看到 A→B(双时态历史);compaction 后物理清除
方向:neighbors(A) 只含 A 的出边,predecessors(B) 含指向 B 的入边;
      RelationIndex::Outgoing 与 Both 下两者结果一致(反向索引只加速、不改语义)
```

### 3.7 反馈幂等(I27)

```text
同一 (rowid, query_id) 重复 feedback(Used) n 次 → access_count 只 +1;
不同 query_id 则各自计数;崩溃后重放不重复计分;
不可见记录(不存在/已墓碑/已过期)feedback → false 且不占用幂等键——
该键随后对可见记录仍生效(FC-SCORE-INV-027)
```

---

## 4. 性能基准(criterion;L3 起夜间跑)

| 基准 | 场景 | 门槛(默认参数) |
|---|---|---|
| 建库吞吐 | 批量插入 1M×1536,并行构建 | ≥ 50k 向量/秒(768 维口径 ≥ 100k) |
| 查询延迟 | 1M×1536,i8 量化,ef=128 | P50 < 2ms,**P99 < 10ms** |
| 召回-延迟曲线 | ef ∈ {32..512} | ef=128 时 Recall@10 ≥ 0.95 |
| 过滤三档 | 选择性 50% / 5% / 0.05% | 各档延迟与等价性达标 |
| 混合检索 | 向量+BM25 RRF | 融合开销 < 1ms |
| 冷启动 | open 1M 条(目标:hidx mmap 惰性;当前未兑现) | < 1s |
| compaction 停顿 | 合并期间并发查询 | P99 抬升 < 30% |
| 量化收益 | f32 vs i8 | ≥ 3× 加速且召回损失 ≤ 2% |

> **L3 落地状态**:`benches/hnsw.rs` 已提供建库吞吐与查询延迟两项 criterion 基准
> (当前为 1k/8k×64 维微缩样本,1M×1536 门槛待 heavy 档);召回门槛由
> `tests/hnsw_contracts.rs` 以微缩双分布验收(见 §3)。**冷启动门槛尚未兑现**——L3 的
> mmap 只是段读取路径优化,恢复仍整段载入,真正"惰性驻留"待 L5/L6 段句柄重构
> (见 [05 §10](05-l3-hnsw.md))。

基线入库(`benches/` + 夜间趋势图),回归 > 10% 阻断合并。

**复杂度契约**:本节门槛同时验收 [spec/contracts.md §9](../spec/contracts.md) 的
`FC-*-CPLX-*` 条目;复杂度**渐进**退化(如 $O(\log k)\to O(k)$)即使基准回归 < 10%
也阻断合并,并须先更新对应 `CPLX` 契约(契约优先)。

---

## 5. Fuzz(I7,I18 的鲁棒性;L2/L4;L6 起 nightly)

cargo-fuzz 目标:`fuzz_vsec`、`fuzz_msec`、`fuzz_hidx`、`fuzz_wal_replay`、`fuzz_dsl`。
断言统一:**任意输入不 panic、不 UB、不无限循环**;解析失败必须返回结构化错误
(I7:DSL 任意输入不 panic)。

**版本注入**(I18):在上述解码目标中随机改写文件头 `format_version` 的**主版本**为更大值,
断言返回 `UnsupportedVersion` 而非继续解析;改写为魔数不符的值,断言 `Corrupted`。

发布前本地连续跑:每个目标 ≥ 1h,且全部目标累计 ≥ 24h(可分多轮累计)。

### 5.1 确定性时间测试(Clock)

用假时钟而非 `sleep` 验证时间语义,消除 flaky:

```text
TTL:     注入 expires_at = T;时钟拨到 T-1 → 可见;拨到 T → 不可见(逻辑过期)
遗忘:    半衰期 14 天,快进 28 天 → retain 按 07 §3.4 算例逐条断言
时钟回拨: 时钟从 T 拨回 T-Δ → 记录不得被误判过期(04 §10.2 单调钳制)
```

### 5.2 三值逻辑与 DSL 边界(FC-QUERY-ERR-002)

```text
缺失字段: not(kind=="x") 不命中无 kind 字段的记录;exists(kind) 命中;
is_null(x) 仅命中显式 null;contains/startswith/endswith/~ 对缺失字段为 false
fuzz_dsl 补充:任意输入不 panic、错误带位置(I7)
```

### 5.3 加密与压缩(I28)

```text
开启加密写入 → 扫描所有段/WAL/MANIFEST 字节,断言不含明文 text/key 子串;
翻转密文 1 bit → 读取返回 Corrupted,绝不返回错误数据;
错误密钥 → Corrupted;密钥轮换后新旧均可读
压缩:roundtrip 等价;低于阈值自动存原文
```

---

## 6. 长跑测试(I8–I11,I17,I26 的验收)

```text
场景: 模拟 Agent 日常 —— 随机命名空间 20 个,持续写入(90% 短 TTL,
      10% 长期记忆)+ 周期 update/supersede + 周期检索 + 周期 retain + 每 2h 快照备份
时长: 24h(CI 每周), 断言:
  I8  活跃段数曲线 ≤ (T−1)·log_r(N/B)+c(实测画图);WAL ≤ 256MB
  RSS 有界(无泄漏, 页缓存之外的进程内存平稳)
  I9  随机抽样: 过期/墓碑记录零返回
  I10 每轮 compaction 前后: check() 通过, 记录总数对账
  I11 每份备份独立 open + check 全绿
  I26 随机历史时间戳: 多轮 compaction 后 as_of(t) 结果不变(默认永久保留);
      版本链长度/磁盘随 update 数线性增长, history_horizon 有限时按窗口回落
```

### 6.1 快照一致性(I17)

```text
取 SnapshotHandle → 后台持续写入 + 触发多轮 compaction →
  快照上的重复查询结果始终不变,且 ≡ 取快照时刻的暴力参照;
  快照 Drop 后其引用的段文件才可被物理删除(trash 延迟回收, 04 §9)
```

### 6.2 遗忘审计(I23)

```text
默认配置:运行 30 天(假时钟)无 retain 触发,记录零丢失;
显式开启 retain:forgotten 数与 sampled_ids 与实际墓碑一致;
iter_with(None, true) 可导出被删记录(墓碑在 history_horizon 内保留,默认永久)
```

### 6.3 双时态历史读(I26)

```text
写入旧版(valid 2024-01) → supersede 新版(valid 2024-06) → as_of(2024-03) 返回旧版;
as_of(t) 的结果在后续写入与**多轮 compaction 后均不变**(默认永久保留,I26);
显式设 history_horizon=90d → 快进 91 天再 compaction 后,更早版本不可回溯(FC-MODEL-POST-004);
valid_time 过期不触发物理删除
```

### 6.4 只读共享(I29)

```text
写进程持续写入 + 后台 compaction;只读进程并发查询:
  任意时刻结果 = 某已提交 MANIFEST 版本的完整视图,无半提交可见;
  写进程崩溃后只读进程仍可读到最后一个提交版本
```

### 6.5 Observer 隔离(I30)

```text
注册一个会 panic 的 Observer → 所有读写仍成功,引擎状态不变;
事件字段与实际操作一致(段数/字节/耗时单调)
```

---

## 7. CI 结构(GitLab CI)

| 阶段 | 内容 | 触发 |
|---|---|---|
| fast | fmt + clippy(-D warnings)+ 单测 + L1 集成 | 每次推送 |
| middle | L2 崩溃抽样 + L3 召回 + L4 集成 | 每次 MR |
| heavy | 全量崩溃注入 + criterion + 长跑 | 每夜/每周 |
| fuzz | cargo-fuzz | 每夜 |
| mutation | `cargo-mutants`(配置见根目录 `mutants.toml`):存活变异体 = 约束遗漏测试,须补测试后重跑 | 计划(L2 起,未接线) |

矩阵平台:Linux(x86_64/aarch64)+ Windows(x86_64,重点覆盖
[04 §6](04-l2-persist.md)/[04 §9](04-l2-persist.md) 的平台专项)。

> **落地状态**:仓库**尚未提交 CI 配置文件**,上表为规划结构;`cargo-mutants` 与全量
> 崩溃前缀属性测试尚未接线,L2 目前只有基于 `FsyncHook` 的定向崩溃测试(§2)。

---

## 8. 验收即文档

每个测试文件头部列出其覆盖的不变量编号;`db.check()` 在生产环境复用测试的
同一套校验器(同一份代码,避免"测试里一套、生产一套")。

**契约追溯(FSVDD 强制)**:[spec/contracts.md](../spec/contracts.md) 是形式化约束的
唯一真实数据源;每个测试注释必须引用其 `FC-*` 编号。`tests/contract_traceability.rs`
在 `cargo test` 中机械校验"契约条目 ↔ 测试"双向映射(无悬空引用、无孤立测试),
任何新增业务逻辑若未登记契约即阻断合并。

## 本章小结

- 测试金字塔:单元 / 属性 / 集成 / 长跑;不变量编号是测试断言的锚点。
- 崩溃注入用 `FsyncHook` + **前缀不变量**,覆盖撕裂写与位翻转。
- 召回/等价性用属性测试;性能有明确门槛;fuzz 保证不 panic/不 UB。
- CI 四档 + 平台矩阵;`db.check()` 与生产复用同一套校验器。
- 契约追溯由 `tests/contract_traceability.rs` 在 `cargo test` 中强制双向映射。

## 下一章

[15-glossary.md](15-glossary.md):术语、符号与复杂度速查。
