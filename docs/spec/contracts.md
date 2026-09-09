# Mneme 形式化契约矩阵(FC-Matrix)

> 本文件是 Mneme 的**形式化约束唯一真实数据源**(Single Source of Truth)。
> 它把散落在各章的 Pre / Post / Invariant / State / Error / Complexity 约束收敛为可追溯条目,
> 并建立"契约 ↔ 测试"1:1 映射(FSVDD 强制项)。
>
> - 维护协议见仓库根目录的 `CONTRIBUTING.md`「契约维护」;
> - 不变量正文定义见各章末尾与 [14 测试验收](../design/14-testing.md);
> - 编号规则:`FC-<模块>-<类型>-<序号>`,类型 ∈ {PRE, POST, INV, STA, ERR, CPLX}。
> - 状态列:`Planned`(已定义、待实现)/ `Passed` / `Failed` / `Waived`(附理由)。

---

## 0. 图例与约定

| 符号 | 含义 |
|---|---|
| PRE | 前置条件(P_pre) |
| POST | 后置条件(P_post) |
| INV | 系统不变量(I_sys) |
| STA | 状态机转移约束(M) |
| ERR | 异常/边界语义(E) |
| CPLX | 算法复杂度/资源约束(时间与空间渐近上界;POST 的资源特化,见 §0.1) |

### 0.1 CPLX 与 FSVDD 五维的关系

`CPLX` 不是独立维度,而是后置约束 `P_post` 的**资源特化**:它约束的不是返回值,
而是操作在给定输入规模下的时间/空间开销(FSVDD §2.2「副作用与输出契约」的资源边界)。
单独编号的目的,是让算法复杂度成为**可追溯、可回归、可门禁**的第一类契约——
既满足「算法复杂度由契约保证」,又不破坏 Pre / Post / Invariant / State / Error 五维骨架。

> **`待补` 约定**:「对应测试 / 基准」列标 `待补` 表示该契约**尚无实现的测试/基准**;
> 实现并通过后必须回填真实路径并把状态改为 `Passed`,**不得预先编造文件名占位**。
> 已实现且可追溯的条目直接引用真实测试路径(如 `tests/core_contracts.rs::*`)。

破坏性变更需记录在下方变更记录。

### 变更记录

| 日期 | 变更 |
|---|---|
| 2026-09 | 新增 `CPLX` 复杂度契约类型与 §9「算法复杂度契约」:为 L0–L6 全层逐操作登记时间/空间渐近上界,并定义验证方式、口径标注与复杂度回归门禁 |
| 2026-09 | 测试追溯口径修订:未实现测试/基准的契约统一标 `待补`,不再预填未落地的测试文件名;已实现的 L0 契约引用真实 `tests/core_contracts.rs::*` |
| 2026-09 | 新增 I19–I30:delta/注册/BM25 全局/稳定 RowId/加密/只读等契约 |
| 2026-09 | 口径修订:统一 I21(BM25 统计一致性);新增 FC-INDEX-POST-003(排序全等性) |
| 2026-09 | 设计变更:引入 RowId 版本链,`as_of`/`supersede` 历史默认永久保留(受 `history_horizon` 约束);重写 I26,新增 FC-MODEL-POST-004 |
| 2026-09 | 补充:`predecessors` 入边 API(FC-MODEL-POST-005)、写入期去重语义(FC-INDEX-POST-004);段状态统一为 `Building` |
| 2026-09 | 新增 L0 原语层契约(FC-CORE-*):类型/度量/SIMD/TopK/varint/meta |
| 2026-09 | 澄清 L0 层边界:`Encryption`/`KeyProvider`/`Cipher` 归属 L11(移出 02 §8/§9);`MnemeError` 标注 `#[non_exhaustive]`;SIMD 明确非对齐加载 + `avx2`/`fma` 双检测;`cosine`/`euclidean_sq` 定位于 `metric` |
| 2026-09 | 对齐 L0 文档与实现:修正 FC-CORE-ERR-002 阈值口径为 `‖a‖·‖b‖ < ε`(非范数平方乘积);varint 解码命名统一为 `decode_u32`/`decode_u64` |
| 2026-09 | L0 一致性审计:`cosine` rustdoc 阈值口径统一为 `‖a‖·‖b‖ < ε`;FC-CORE-INV-002 明确 `simd::dot` 等长前置条件;04 §2.1 的 32B 对齐由"要求"改为"优化项";为直接依赖 `serde` 补充用途注释 |
| 2026-09 | L0 规范对齐:rustdoc 补全 Arguments/Returns/Examples;`options.rs` 拆分为按主题的 `options/` 模块目录(公共 API 路径不变,非破坏性);varint 协议常量具名化;登记证伪原则落地口径(§10 第 6 条) |

---

## 1. 原语层(core / L0)

> 无 I/O、无全局状态、无锁的纯类型与纯函数;唯一 `unsafe` 在 `simd.rs` 的 arch 内联。
> 层边界契约见 [02 §9](../design/02-l0-core.md)。

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-CORE-PRE-001 | PRE | `Dimension::new(d)` 仅接受 `d ∈ [1, 65536]`;越界返回 `Invalid`,内部不再使用裸整数 | `tests/core_contracts.rs::dimension_bounds` | Passed |
| FC-CORE-POST-001 | POST | `Metric::score` 严格映射:Dot = a·b;Cosine = a·b / √(a_norm·b_norm);Euclidean = a_norm + b_norm − 2·a·b(其中 norm 为**范数平方**) | `tests/core_contracts.rs::metric_score_mapping` | Passed |
| FC-CORE-POST-002 | POST | `Metric::better(x,y)` 给出统一方向:Cosine/Dot 分数越大越优,Euclidean 越小越优;TopK/归并/排序一律经此比较 | `tests/core_contracts.rs::metric_better_direction` | Passed |
| FC-CORE-POST-003 | POST | `Metric::needs_norm()` 仅 `Dot` 返回 `false`;`Cosine`/`Euclidean` 返回 `true` | `tests/core_contracts.rs::metric_needs_norm` | Passed |
| FC-CORE-POST-004 | POST | `TopK<T: Ord>` 恒保留按 `better` 方向最优的 k 个;同分按载荷升序稳定;`merge` 结果 ≡ 顺序 `push` 全部元素;`into_sorted_vec` 最优在前 | `tests/core_contracts.rs::topk_equivalence` | Passed |
| FC-CORE-POST-005 | POST | varint 编解码往返:`decode_u64(encode_u64(x)) == (x, len)`、`decode_u32(encode_u32(x)) == (x, len)` 且编码为最小编码(无多余 continuation 字节) | `tests/core_contracts.rs::varint_roundtrip_minimal` | Passed |
| FC-CORE-POST-006 | POST | `meta::get_path` 按 `.` 分段遍历对象,缺失/类型不符返回 `None`;`as_f64/as_i64/as_bool/as_str/as_ts` 仅匹配对应 JSON 类型 | `tests/core_contracts.rs::meta_accessors` | Passed |
| FC-CORE-POST-007 | POST | `RelationKind` 内置常量 `DERIVED_FROM=0`、`SUPPORTS=1`、`CONTRADICTS=2`、`RELATED=3`;自定义编号从 16 起 | `tests/core_contracts.rs::relation_kind_builtins` | Passed |
| FC-CORE-INV-001 | INV | `simd::dot(a,b)` 与标量参考实现等价(容差内),覆盖长度非 LANE 倍数与空切片 | `tests/core_contracts.rs::dot_matches_scalar_reference` | Passed |
| FC-CORE-INV-002 | INV | L0 公开 API 在其定义域内对任意输入不 panic、无 UB(畸形 varint、空向量、越界维度等);`simd::dot` / `Metric::score` 要求两切片等长,长度不等属调用方违约(debug 断言,release 按较短者计算) | `tests/core_contracts.rs::no_panic` | Passed |
| FC-CORE-ERR-001 | ERR | varint 解码遇截断或超长(> 10 字节)返回结构化 `Corrupted`,绝不 panic、绝不静默跳过 | `tests/core_contracts.rs::varint_malformed` | Passed |
| FC-CORE-ERR-002 | ERR | 余弦分母 `√(a_norm·b_norm) = ‖a‖·‖b‖ < ε`(零向量,ε=1e-12)时返回 `0`,绝不返回 `NaN` | `tests/core_contracts.rs::cosine_zero_vector` | Passed |

---

## 2. 持久层(persist)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-PERSIST-INV-001 | INV | **I1**:已确认写入不半写;未确认写入重启后要么完整可见要么不存在 | 待补 | Planned |
| FC-PERSIST-INV-002 | INV | **I2**:任意 bit 损坏可检出或拒绝启动,绝不静默返回错误数据 | 待补 | Planned |
| FC-PERSIST-INV-003 | INV | **I3**:活跃段集合 = 某 MANIFEST 版本所列集合 | 待补 | Planned |
| FC-PERSIST-INV-004 | INV | **I4**:WAL 总量 ≤ `wal_bytes`;段文件只增不改 | 待补 | Planned |
| FC-PERSIST-INV-019 | INV | **I19**:`delete`/`update`/`touch`/`relate` 返回 `Ok` 后,崩溃 + WAL 截断仍生效,删除永不复活 | 待补 | Planned |
| FC-PERSIST-INV-020 | INV | **I20**:`path↔NsId`、`next_ns_id`、`next_rowid` 可由 MANIFEST+WAL 重建,ID 永不复用 | 待补 | Planned |
| FC-PERSIST-POST-001 | POST | **I15**:`insert_batch` 整批原子:可见记录数 ∈ {0, n},无部分批 | 待补 | Planned |
| FC-PERSIST-POST-002 | POST | Checkpoint 仅当 `seqno ≤ watermark` 的覆盖条目已物化时才截断 WAL | 待补 | Planned |
| FC-PERSIST-POST-003 | POST | **I16**:`close()` 返回 `Ok` 后所有已确认写入持久;`Drop` 不保证 | 待补 | Planned |
| FC-PERSIST-ERR-001 | ERR | 未知 WAL 帧类型 → 停止回放并报错,不静默跳过 | 待补 | Planned |
| FC-PERSIST-ERR-002 | ERR | 更高主版本 → `UnsupportedVersion`(I18) | 待补 | Planned |
| FC-PERSIST-STA-001 | STA | 段生命周期:`Building → Committed → Obsolete → (trash)`;`Committed` 段内容不可变 | 待补 | Planned |
| FC-PERSIST-STA-002 | STA | 崩溃点状态:`Building` 段为孤儿,恢复时清理;不进入任何 MANIFEST 视图 | 待补 | Planned |

---

## 3. 索引与检索(index/query/score)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-INDEX-INV-005 | INV | **I5**:同一快照内 `execute()` = 候选集内暴力 + 标准融合(统计等价) | 待补 | Planned |
| FC-INDEX-INV-006 | INV | **I6**:过滤先行;结果与融合顺序无关 | 待补 | Planned |
| FC-INDEX-INV-021 | INV | **I21**:BM25 统计按查询命名空间跨全部活跃段全局聚合(df/N/avgdl),只计活行,与段数无关,跨 NS 互不影响 | 待补 | Planned |
| FC-INDEX-POST-001 | POST | 过滤三档结果 ≡ 候选位图内暴力(集合相等) | 待补 | Planned |
| FC-INDEX-POST-002 | POST | `ef → ∞` 时 HNSW 结果收敛于精确暴力 | 待补 | Planned |
| FC-INDEX-POST-003 | POST | **排序全等性**:同一快照内任意两次 `execute()`(同参数)结果完全一致(同分按 RowId 升序) | 待补 | Planned |
| FC-SCORE-INV-027 | INV | **I27**:同一 `(rowid, query_id)` 的反馈至多计一次 | 待补 | Planned |
| FC-SCORE-POST-001 | POST | `Scoring::default()` 与未开启 `score()` 的排序全等 | 待补 | Planned |
| FC-SCORE-POST-002 | POST | `Scoring::floor` 下的候选满足 `ŝ ≥ floor` 或 `S = 0` | 待补 | Planned |
| FC-SCORE-POST-003 | POST | 放大 ef 后综合排序相对召回损失 ≤ 2% | 待补 | Planned |
| FC-QUERY-ERR-001 | ERR | DSL 任意输入不 panic,返回结构化 `FilterParse`(I7) | 待补 | Planned |
| FC-QUERY-ERR-002 | ERR | `Not` 对缺失字段采用三值语义(缺失 → `Not` 亦为 false) | 待补 | Planned |
| FC-INDEX-POST-004 | POST | 写入期去重:`Dedup::Merge` 就地更新并保留旧 RowId(返回 `Merged(old)`);`Dedup::Replace` 生成新 RowId 并墓碑旧行;`insert_batch` 中 `RejectDuplicate`/`Dedup::Reject` 逐条返回 `Duplicate`,不回滚整批 | 待补 | Planned |

---

## 4. 记忆模型(model)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-MODEL-INV-022 | INV | **I22**:RowId 跨 `update`/upsert 不变,访问统计与关系边始终有效 | 待补 | Planned |
| FC-MODEL-INV-024 | INV | **I24**:`update` 新版本对读者原子可见,旧版本立即遮蔽 | 待补 | Planned |
| FC-MODEL-INV-025 | INV | **I25**:悬挂边不可见;删除一端后边立即失效,compaction 后物理清除 | 待补 | Planned |
| FC-MODEL-INV-026 | INV | **I26**:`as_of(t)` = 事务时间 ≤ t 的最新可见版本组成的一致快照,不随后续写入/compaction 变化;历史版本默认永久保留(`history_horizon=None`) | 待补 | Planned |
| FC-MODEL-POST-001 | POST | `relate` / `relate_with_meta` 以 `(from,to,kind)` 幂等 upsert(后者同时覆盖 metadata) | 待补 | Planned |
| FC-MODEL-POST-002 | POST | `consolidate` 幂等:已沉淀簇跳过;`keep_sources=true` 不删除来源 | 待补 | Planned |
| FC-MODEL-POST-003 | POST | `supersede` 后旧版本 `valid_to` = 新版本 `valid_from`(历史可见性受 compaction 回收约束,I26) | 待补 | Planned |
| FC-MODEL-POST-004 | POST | **版本链保留**:每个 RowId 的最新版本与 `history_horizon` 内的历史版本被保留;仅超期版本被回收,`as_of` 在窗口内可读 | 待补 | Planned |
| FC-MODEL-POST-005 | POST | `predecessors(to, kinds)` 只返回 `edge.to == to` 且 `edge.kind ∈ kinds`、两端存活的边;`RelationIndex::Outgoing` 与 `Both` 结果一致(反向索引只加速、不改语义) | 待补 | Planned |
| FC-MODEL-STA-001 | STA | 记忆版本:`Active(seqno_max,当前可见) → Shadowed(被遮蔽,仅 `as_of` 可见) → Reclaimed(超出 `history_horizon` 后物理回收)`;`Shadowed` 不可作为当前查询结果 | 待补 | Planned |

---

## 5. 生命周期(life)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-LIFE-INV-008 | INV | **I8**:活跃段数 ≤ `(T−1)·log_r(N/B)+c`;WAL ≤ `wal_bytes` | 待补 | Planned |
| FC-LIFE-INV-009 | INV | **I9**:逻辑过期/墓碑记录在常规读路径永不返回(仅 `iter_with(..., true)` 审计入口可见);物理回收仅在 compaction 提交后 | 待补 | Planned |
| FC-LIFE-INV-010 | INV | **I10**:compaction 崩溃 → 恢复后 = 提交前状态(孤儿段清理) | 待补 | Planned |
| FC-LIFE-INV-011 | INV | **I11**:备份目录独立 `open` + `check` 通过 | 待补 | Planned |
| FC-LIFE-INV-017 | INV | **I17**:`SnapshotHandle` 视图一致,后台 compaction 不影响 | 待补 | Planned |
| FC-LIFE-INV-023 | INV | **I23**:自动遗忘默认关闭;删除可审计(墓碑在 `history_horizon` 内保留,默认永久,经 `iter_with(..., true)` 可见),绝不静默 | 待补 | Planned |
| FC-LIFE-POST-001 | POST | `retain` 返回 `forgotten` 与 `sampled_ids` 与实际墓碑一致 | 待补 | Planned |

---

## 6. 量化与门面(quant)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-QUANT-INV-012 | INV | **I12**:量化模式 `Hit.score` = f32 精排分 | 待补 | Planned |
| FC-QUANT-INV-013 | INV | **I13**:召回不达标自动回退 f32,`stats()` 可见 | 待补 | Planned |
| FC-QUANT-INV-014 | INV | **I14**:async 与 sync API 等价(共享同一写锁) | 待补 | Planned |
| FC-QUANT-ERR-001 | ERR | 未开 `quant-f16` 时 `VectorFormat::F16` 构造期返回 `Invalid`,不静默降级 | 待补 | Planned |

---

## 7. 安全与部署(security/deploy)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-SEC-INV-028 | INV | **I28**:开启加密后磁盘无明文记录字段;认证失败 → `Corrupted` | 待补 | Planned |
| FC-SEC-POST-001 | POST | 密钥轮换后新旧密钥均可读,迁移完成旧 key 退役 | 待补 | Planned |
| FC-DEPLOY-INV-029 | INV | **I29**:只读实例看到的始终是某已提交 MANIFEST 版本的完整视图 | 待补 | Planned |
| FC-DEPLOY-INV-030 | INV | **I30**:`Observer` 回调不改变引擎行为;回调 panic 被隔离 | 待补 | Planned |
| FC-DEPLOY-STA-001 | STA | 只读视图切换:`V_n → V_{n+1}` 原子;不存在中间态 | 待补 | Planned |

---

## 8. 边界与极值(全局)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-GLOBAL-PRE-001 | PRE | 向量维度 ∈ [1, 65536];长度 ≠ 建库维度 → `DimensionMismatch` | 待补 | Planned |
| FC-GLOBAL-PRE-002 | PRE | 任一分量 `NaN`/`±Inf` → `Invalid`,库内不被污染 | 待补 | Planned |
| FC-GLOBAL-PRE-003 | PRE | key ≤ 1024B、text ≤ 1MiB、meta ≤ 64KiB、深度 ≤ 32 → 否则 `TooLarge` | 待补 | Planned |
| FC-GLOBAL-PRE-004 | PRE | `importance`/`confidence` 越界钳制到 [0,1];`top_k`/`ef` ≤ 4096 | 待补 | Planned |
| FC-GLOBAL-ERR-001 | ERR | 库绝不 panic;`filter!` 字面量与 async `spawn_blocking` 为文档化例外 | 待补 | Planned |
| FC-GLOBAL-PRE-005 | PRE | 时钟回拨经单调水位钳制;记录不会因回拨早消失 | 待补 | Planned |

---

## 9. 算法复杂度契约(CPLX)

> 复杂度是**资源后置约束**:每个公开操作在声明的输入规模下,时间与空间开销必须落在
> 下表上界内。所有复杂度以**最坏情况**为默认口径;显式标注「摊还」「期望」者除外。
> 变量定义见 §9.1,验证方式与门禁见 §9.3;与 [15 §3 复杂度速查](../design/15-glossary.md)
> 一一对应,后者是阅读视图,本节是唯一真实数据源。

### 9.1 符号与口径

| 符号 | 含义 |
|---|---|
| $d$ | 向量维度 |
| $N$ | 数据集/段/文档总行数(按上下文) |
| $N_c$ | 过滤后候选行数($N_c \le N$) |
| $k$ | top-k 的 k |
| $n$ | 单个段的行数 |
| $S_{\text{seg}}$ | 活跃段数 |
| $S_{\text{merge}}$ | 单轮 compaction 的数据量 |
| $C_{\text{block}}$ | 并行扫描的块数 |
| $\|\phi\|$ | 过滤谓词 AST 节点数 |
| $L$ | DSL 表达式字符长度 |
| $E$ | 关系边数 |
| $deg$ | 关系图中节点的度数 |
| $B,\ r$ | 段初始行数 / 分级比 |
| $M,\ M_0$ | HNSW 上层 / 第 0 层度数上限 |
| $ef,\ ef_c$ | 查询 / 构建探查宽度 |
| $W_{\text{amp}}$ | 写放大系数($\approx \log_r(N/B)$) |

口径约定:①未标注者默认**最坏情况**;②摊还(amortized)仅用于 compaction/写放大;
③HNSW 查询为**期望上界**(依赖层级随机分布,$ef\to\infty$ 收敛见 FC-INDEX-POST-002),
不得当作确定性最坏界;④空间只计操作**额外**占用,不含调用方传入/持有的数据。

### 9.2 契约表

#### 9.2.1 L0 原语层(core)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-CORE-CPLX-001 | CPLX | `simd::dot` / `dot_scalar` / `Metric::score`:时间 $O(d)$;AVX2 指令数 $\approx 3d/8+3$;空间 $O(1)$ | 待补 | Planned |
| FC-CORE-CPLX-002 | CPLX | `Metric::better` / `needs_norm`:时间 $O(1)$、空间 $O(1)$ | 待补 | Planned |
| FC-CORE-CPLX-003 | CPLX | `TopK::push`:未满 $O(\log k)$、已满 $O(1)$ 拒绝或 $O(\log k)$ 下沉(最坏 $O(\log k)$);`TopK::new` 预分配 $\le \min(k,1024)$;空间 $O(k)$ | 待补 | Planned |
| FC-CORE-CPLX-004 | CPLX | `TopK::merge` / `into_sorted_vec`:时间 $O(k\log k)$,**与 $N$ 无关**;空间 $O(k)$ | 待补 | Planned |
| FC-CORE-CPLX-005 | CPLX | `varint::{encode_u32,encode_u64,decode_u32,decode_u64}`:时间 $O(\lfloor\log_{128}x\rfloor+1)\le 10$ 字节操作;空间 $\le 10$ B | FC-CORE-POST-005 长度断言 | Passed |
| FC-CORE-CPLX-006 | CPLX | `meta::get_path`:时间 $O(p)$($p$ = `.` 分段数,每段平均 $O(1)$ 查找);`as_f64/as_i64/as_bool/as_str/as_ts`: $O(1)$ | 待补 | Planned |

#### 9.2.2 L1 内存层(mem)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-MEM-CPLX-001 | CPLX | 暴力扫描:时间 $O(N\cdot d)$,过滤后 $O(N_c\cdot d)$;空间 $O(N/8)$ 位图 + $O(k)$ | 待补 | Planned |
| FC-MEM-CPLX-002 | CPLX | 元数据过滤求值:时间 $O(N\cdot\|\phi\|)$,单行 $O(\|\phi\|)$(短路求值);空间 $O(N/8)$ | 待补 | Planned |
| FC-MEM-CPLX-003 | CPLX | 并行归并:时间 $O(C_{\text{block}}\cdot k\log k)$;空间 $O(C_{\text{block}}\cdot k)$ | 待补 | Planned |
| FC-MEM-CPLX-004 | CPLX | `delete` / `touch`:时间 $O(\log n)$ 定位 + $O(1)$ 墓碑/统计更新;空间 $O(1)$ | 待补 | Planned |
| FC-MEM-CPLX-005 | CPLX | `iter(filter)`:时间 $O(N_c)$ 流式;空间 $O(1)$(不含调用方收集的结果集) | 待补 | Planned |

#### 9.2.3 L2 持久层(persist)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-PERSIST-CPLX-001 | CPLX | WAL 提交:单条 $O(1)$ 内存追加;组提交 $N$ 条 $O(N)$ 追加 + **1 次** fsync/批;空间顺序写 | 待补 | Planned |
| FC-PERSIST-CPLX-002 | CPLX | WAL 回放:时间 $O(\text{unflushed frames})$;空间 $O(1)$ 流式 | 待补 | Planned |
| FC-PERSIST-CPLX-003 | CPLX | CRC-32:时间 $O(n)$、空间 $O(1)$(8 KiB 常量表);$n$ = 字节数 | 待补 | Planned |
| FC-PERSIST-CPLX-004 | CPLX | zone map 剪枝:时间 $O(\lceil n/1024\rceil \times \text{predicates})$;空间 16 B/块/字段 | 待补 | Planned |
| FC-PERSIST-CPLX-005 | CPLX | bloom 判定:时间 $O(k)=O(7)$;空间 $1.44\log_2(1/p)$ bit/元素 | 待补 | Planned |
| FC-PERSIST-CPLX-006 | CPLX | MANIFEST 提交:时间 $O(S_{\text{seg}})$ 写新文件;空间保留 2 版 | 待补 | Planned |
| FC-PERSIST-CPLX-007 | CPLX | `open` 恢复:时间 $O(\text{WAL replay})$ + 段头校验;mmap 惰性,空间 $O(1)$/段 | 待补 | Planned |
| FC-PERSIST-CPLX-008 | CPLX | 单点写 `insert`:时间 $O(1)$ 内存 + WAL 追加(fsync 按 `FsyncPolicy`);空间 $O(d)$ | 待补 | Planned |
| FC-PERSIST-CPLX-009 | CPLX | 单点读 `get(key)`:时间 $O(\log n)$ + 一次记录读;`get_by_rowid`: $O(\log n)$ 版本链定位 | 待补 | Planned |
| FC-PERSIST-CPLX-010 | CPLX | `as_of(t)`:时间 $O(S_{\text{seg}}\cdot\log n)$ 定位版本链 + 查询;空间随历史窗口增长 | 待补 | Planned |

#### 9.2.4 L3 索引层(index)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-INDEX-CPLX-001 | CPLX | HNSW 单点插入:时间 $O(d\cdot(ef_c\cdot M_0 + M\log_M N))$;构建 $O(N\cdot d\cdot ef_c\cdot M_0)$;空间 $\approx(8M+20)$ B/节点 + 边表 | 待补 | Planned |
| FC-INDEX-CPLX-002 | CPLX | HNSW 查询:期望上界 $O(d\cdot ef\cdot M_0)$(实测 $\approx(2\text{–}5)\cdot ef$ 次点积);空间 $O(ef)$ | 待补 | Planned |
| FC-INDEX-CPLX-003 | CPLX | 上层下降:时间 $O(d\cdot M\cdot\log_M N)$;层高期望 $O(\log_M N)$ | 待补 | Planned |

#### 9.2.5 L4 检索与排序层(query/score)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-QUERY-CPLX-001 | CPLX | DSL 解析:时间 $O(L)$ 单遍;空间 $O(\|\phi\|)$ | 待补 | Planned |
| FC-QUERY-CPLX-002 | CPLX | 计划编译:时间 $O(\text{blocks}\times\text{predicates})$;块位图空间 $O(n/8)$ | 待补 | Planned |
| FC-QUERY-CPLX-003 | CPLX | BM25 打分:时间 $O(2\sum_{t\in Q} df_t)$ postings 访问;空间 $O(\text{postings})$ 静态 | 待补 | Planned |
| FC-QUERY-CPLX-004 | CPLX | RRF/加权融合:时间 $O(k)$、空间 $O(k)$ | 待补 | Planned |
| FC-SCORE-CPLX-001 | CPLX | 综合重排/归一化:时间 $O(m)$($m$ = 候选数),每候选 $O(1)$;空间 $O(m)$ | 待补 | Planned |
| FC-SCORE-CPLX-002 | CPLX | 联想扩展:时间 $O(\text{seeds}\cdot\text{max\_nodes}\cdot\text{avg\_degree})$(有界 BFS,$hops\le 3$);空间 $O(\text{max\_nodes})$ | 待补 | Planned |
| FC-SCORE-CPLX-003 | CPLX | MMR 贪心:时间 $O(k^2)$(冗余相似度缓存后;现算为 $O(k^2\cdot d)$);空间 $O(k)$ | 待补 | Planned |

#### 9.2.6 L5 生命周期层(life)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-LIFE-CPLX-001 | CPLX | TTL 逻辑过期:时间 $O(\text{blocks})$(块级 min 剪枝);空间 8 B/块 | 待补 | Planned |
| FC-LIFE-CPLX-002 | CPLX | `retain` 扫描:时间 $O(N_{\text{cand}})$(元数据级,不读向量);空间 $O(N_{\text{cand}})$ | 待补 | Planned |
| FC-LIFE-CPLX-003 | CPLX | compaction 单轮:时间 $O(S_{\text{merge}}\cdot d\cdot ef_c\cdot M_0)$(建图主导);摊还 $O(d\cdot ef_c\cdot M_0\cdot W_{\text{amp}})$;空间峰值 $+O(S_{\text{merge}})$ | 待补 | Planned |
| FC-LIFE-CPLX-004 | CPLX | 活跃段数 $\le (T-1)\log_r(N/B)+c = O(\log_r N)$(I8);WAL $\le wal\_bytes$ | 待补 | Planned |
| FC-LIFE-CPLX-005 | CPLX | `snapshot`:时间 $O(1)$(clone `Arc` 视图);`backup_to`:同盘 $O(\text{files})$、跨盘 $O(\text{bytes})$;`check`: $O(\text{total bytes})$ | 待补 | Planned |

#### 9.2.7 L6 量化层(quant)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-QUANT-CPLX-001 | CPLX | i8 量化点积:粗排副本带宽 $4d\to d$ B/行(÷4);VNNI 指令再 $\approx 4\times$;空间副本 $d$ B/行(f32 原向量另存) | 待补 | Planned |
| FC-QUANT-CPLX-002 | CPLX | 两阶段检索:粗排候选 $\le$ `rescore_candidates`(默认 4k);精排时间 $O(k\cdot d)$ | 待补 | Planned |

#### 9.2.8 记忆模型 / 安全 / 部署

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-MODEL-CPLX-001 | CPLX | `neighbors(from)`:时间 $O(\log E + degree)$;`predecessors(to)`:默认 $O(E)$ 全段扫描,`RelationIndex::Both` 时 $O(\log E + degree)$;空间 $O(degree)$ | 待补 | Planned |
| FC-SEC-CPLX-001 | CPLX | 加解密:时间 $O(n)$(AES-NI);压缩/解压:时间 $O(n)$、空间 $O(n)$ | 待补 | Planned |
| FC-DEPLOY-CPLX-001 | CPLX | 只读视图切换:时间 $O(1)$(原子交换已构建视图) | 待补 | Planned |

#### 9.2.9 全局(global)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-GLOBAL-CPLX-001 | CPLX | **无隐藏复杂度**:任何公开操作的实际开销不得渐进劣于其声明上界;路径中不得出现未声明的 $O(N)$ 全量扫描 | CI 复杂度门禁(§9.3) | Planned |

### 9.3 验证方式与回归门禁

1. **验证方式三选一**:每条 CPLX 必须由下列至少一种方式验证,并在「对应测试 / 基准」列登记:
   - **操作计数单测**:对结构化操作(堆、位图、postings、版本链)统计比较/访问次数,断言其关于规模的增长率;
   - **criterion 基准门槛**:对吞吐/延迟敏感路径,以 [14 §4](../design/14-testing.md) 门槛为准
     (如 1M×1536 i8 ef=128 P99 < 10ms、构建 ≥ 50k 向量/秒);
   - **解析证明**:纯数学路径在设计章给出推导,基准仅作回归哨兵。
2. **口径必须标注**:最坏 / 摊还 / 期望三者择一;未标注默认最坏。HNSW 查询为期望上界,
   $ef\to\infty$ 收敛性由 FC-INDEX-POST-002 验收。
3. **回归门禁**:基准相对基线回归 > 10% 阻断合并([14 §4](../design/14-testing.md));
   复杂度的**渐进**退化(如 $O(\log k)\to O(k)$)即使常数回归 < 10% 也视为破坏性变更,
   须走 §5.3 版本化流程,不得静默放宽。
4. **禁止隐藏复杂度**:设计意图为 $O(\log n)$ / 期望 $O(1)$ 的操作,必须在基准中给出
   规模—耗时曲线证明其未退化为 $O(N)$(FC-GLOBAL-CPLX-001)。
5. **与不变量/性能承诺的关系**:I8(段数有界)与 [15 §3 性能承诺](../design/15-glossary.md)
   是 CPLX 的宏观汇总;CPLX 是逐操作的微观保证,二者冲突时以更严格者为准。

---

## 10. 追溯规则(FSVDD 强制)

1. **无孤儿实现**:任何新增业务逻辑必须在本矩阵登记至少一条约束;
2. **无失效契约**:本矩阵条目若与代码不符,以"先改契约、再改测试、再改代码"为准;
3. **无孤立测试**:每个测试文件头部注释必须引用其覆盖的 `FC-*` 编号;
4. **100% 映射**:本矩阵条目数与测试套件条目数一一对应,CI 校验
   (`xtask check-contracts`,见 [14 §8](../design/14-testing.md));
5. **豁免**:纯文档改动不新增契约;但涉及磁盘格式/API 语义的文档改动必须先更新本矩阵;
6. **证伪原则落地口径**:每条 ERR / INV 契约均配备专项失败测试——放宽任一约束
   (如 varint 超长校验、余弦 ε 阈值、`Dimension` 边界)必有一个测试变红;
   机械化变异测试(`cargo-mutants`)列入 L2 阶段 CI 任务(见 [14 §7](../design/14-testing.md));
7. **复杂度可追溯**:每条 `CPLX` 契约必须有对应的基准 / 操作计数测试(§9.3);
   任何使复杂度渐进退化的改动,必须先更新 `CPLX` 契约,再改测试与代码。
