# Mneme 形式化契约矩阵(FC-Matrix)

> 本文件是 Mneme 的**形式化约束唯一真实数据源**(Single Source of Truth)。
> 它把散落在各章的 Pre / Post / Invariant / State / Error 约束收敛为可追溯条目,
> 并建立"契约 ↔ 测试"1:1 映射(FSVDD 强制项)。
>
> - 维护协议见仓库根目录的 `CONTRIBUTING.md`「契约维护」;
> - 不变量正文定义见各章末尾与 [14 测试验收](../design/14-testing.md);
> - 编号规则:`FC-<模块>-<类型>-<序号>`,类型 ∈ {PRE, POST, INV, STA, ERR}。
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

破坏性变更需记录在下方变更记录。

### 变更记录

| 日期 | 变更 |
|---|---|
| 2026-09 | 新增 I19–I30:delta/注册/BM25 全局/稳定 RowId/加密/只读等契约 |
| 2026-09 | 口径修订:统一 I21(BM25 统计一致性);新增 FC-INDEX-POST-003(排序全等性) |
| 2026-09 | 设计变更:引入 RowId 版本链,`as_of`/`supersede` 历史默认永久保留(受 `history_horizon` 约束);重写 I26,新增 FC-MODEL-POST-004 |
| 2026-09 | 补充:`predecessors` 入边 API(FC-MODEL-POST-005)、写入期去重语义(FC-INDEX-POST-004);段状态统一为 `Building` |
| 2026-09 | 新增 L0 原语层契约(FC-CORE-*):类型/度量/SIMD/TopK/varint/meta |

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
| FC-CORE-POST-005 | POST | varint 编解码往返:`decode(encode(x)) == x` 且编码为最小编码(无多余 continuation 字节) | `tests/core_contracts.rs::varint_roundtrip_minimal` | Passed |
| FC-CORE-POST-006 | POST | `meta::get_path` 按 `.` 分段遍历对象,缺失/类型不符返回 `None`;`as_f64/as_i64/as_bool/as_str/as_ts` 仅匹配对应 JSON 类型 | `tests/core_contracts.rs::meta_accessors` | Passed |
| FC-CORE-POST-007 | POST | `RelationKind` 内置常量 `DERIVED_FROM=0`、`SUPPORTS=1`、`CONTRADICTS=2`、`RELATED=3`;自定义编号从 16 起 | `tests/core_contracts.rs::relation_kind_builtins` | Passed |
| FC-CORE-INV-001 | INV | `simd::dot(a,b)` 与标量参考实现等价(容差内),覆盖长度非 LANE 倍数与空切片 | `tests/core_contracts.rs::dot_matches_scalar_reference` | Passed |
| FC-CORE-INV-002 | INV | L0 公开 API 对任意输入不 panic、无 UB(畸形 varint、空向量、越界维度等) | `tests/core_contracts.rs::no_panic` | Passed |
| FC-CORE-ERR-001 | ERR | varint 解码遇截断或超长(> 10 字节)返回结构化 `Corrupted`,绝不 panic、绝不静默跳过 | `tests/core_contracts.rs::varint_malformed` | Passed |
| FC-CORE-ERR-002 | ERR | 余弦分母 `a_norm·b_norm < ε`(零向量)时返回 `0`,绝不返回 `NaN` | `tests/core_contracts.rs::cosine_zero_vector` | Passed |

---

## 2. 持久层(persist)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-PERSIST-INV-001 | INV | **I1**:已确认写入不半写;未确认写入重启后要么完整可见要么不存在 | `tests/crash_prefix.rs` | Planned |
| FC-PERSIST-INV-002 | INV | **I2**:任意 bit 损坏可检出或拒绝启动,绝不静默返回错误数据 | `tests/corruption.rs` | Planned |
| FC-PERSIST-INV-003 | INV | **I3**:活跃段集合 = 某 MANIFEST 版本所列集合 | `tests/manifest_atomic.rs` | Planned |
| FC-PERSIST-INV-004 | INV | **I4**:WAL 总量 ≤ `wal_bytes`;段文件只增不改 | `tests/wal_bounded.rs` | Planned |
| FC-PERSIST-INV-019 | INV | **I19**:`delete`/`update`/`touch`/`relate` 返回 `Ok` 后,崩溃 + WAL 截断仍生效,删除永不复活 | `tests/overlay_durability.rs` | Planned |
| FC-PERSIST-INV-020 | INV | **I20**:`path↔NsId`、`next_ns_id`、`next_rowid` 可由 MANIFEST+WAL 重建,ID 永不复用 | `tests/ns_registry_recovery.rs` | Planned |
| FC-PERSIST-POST-001 | POST | **I15**:`insert_batch` 整批原子:可见记录数 ∈ {0, n},无部分批 | `tests/batch_atomic.rs` | Planned |
| FC-PERSIST-POST-002 | POST | Checkpoint 仅当 `seqno ≤ watermark` 的覆盖条目已物化时才截断 WAL | `tests/checkpoint_safety.rs` | Planned |
| FC-PERSIST-POST-003 | POST | **I16**:`close()` 返回 `Ok` 后所有已确认写入持久;`Drop` 不保证 | `tests/close_durability.rs` | Planned |
| FC-PERSIST-ERR-001 | ERR | 未知 WAL 帧类型 → 停止回放并报错,不静默跳过 | `tests/wal_unknown_frame.rs` | Planned |
| FC-PERSIST-ERR-002 | ERR | 更高主版本 → `UnsupportedVersion`(I18) | `fuzz/fuzz_vsec.rs` + 版本注入 | Planned |
| FC-PERSIST-STA-001 | STA | 段生命周期:`Building → Committed → Obsolete → (trash)`;`Committed` 段内容不可变 | `tests/segment_lifecycle.rs` | Planned |
| FC-PERSIST-STA-002 | STA | 崩溃点状态:`Building` 段为孤儿,恢复时清理;不进入任何 MANIFEST 视图 | `tests/orphan_cleanup.rs` | Planned |

---

## 3. 索引与检索(index/query/score)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-INDEX-INV-005 | INV | **I5**:同一快照内 `execute()` = 候选集内暴力 + 标准融合(统计等价) | `tests/recall_property.rs` | Planned |
| FC-INDEX-INV-006 | INV | **I6**:过滤先行;结果与融合顺序无关 | `tests/filter_order.rs` | Planned |
| FC-INDEX-INV-021 | INV | **I21**:BM25 统计按查询命名空间跨全部活跃段全局聚合(df/N/avgdl),只计活行,与段数无关,跨 NS 互不影响 | `tests/bm25_global.rs` | Planned |
| FC-INDEX-POST-001 | POST | 过滤三档结果 ≡ 候选位图内暴力(集合相等) | `tests/filter_tiers.rs` | Planned |
| FC-INDEX-POST-002 | POST | `ef → ∞` 时 HNSW 结果收敛于精确暴力 | `tests/ann_convergence.rs` | Planned |
| FC-INDEX-POST-003 | POST | **排序全等性**:同一快照内任意两次 `execute()`(同参数)结果完全一致(同分按 RowId 升序) | `tests/snapshot_totality.rs` | Planned |
| FC-SCORE-INV-027 | INV | **I27**:同一 `(rowid, query_id)` 的反馈至多计一次 | `tests/feedback_idempotent.rs` | Planned |
| FC-SCORE-POST-001 | POST | `Scoring::default()` 与未开启 `score()` 的排序全等 | `tests/scoring_default_equiv.rs` | Planned |
| FC-SCORE-POST-002 | POST | `Scoring::floor` 下的候选满足 `ŝ ≥ floor` 或 `S = 0` | `tests/scoring_floor.rs` | Planned |
| FC-SCORE-POST-003 | POST | 放大 ef 后综合排序相对召回损失 ≤ 2% | `benches/recall_scoring.rs` | Planned |
| FC-QUERY-ERR-001 | ERR | DSL 任意输入不 panic,返回结构化 `FilterParse`(I7) | `fuzz/fuzz_dsl.rs` | Planned |
| FC-QUERY-ERR-002 | ERR | `Not` 对缺失字段采用三值语义(缺失 → `Not` 亦为 false) | `tests/three_valued_logic.rs` | Planned |
| FC-INDEX-POST-004 | POST | 写入期去重:`Dedup::Merge` 就地更新并保留旧 RowId(返回 `Merged(old)`);`Dedup::Replace` 生成新 RowId 并墓碑旧行;`insert_batch` 中 `RejectDuplicate`/`Dedup::Reject` 逐条返回 `Duplicate`,不回滚整批 | `tests/dedup_semantics.rs` | Planned |

---

## 4. 记忆模型(model)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-MODEL-INV-022 | INV | **I22**:RowId 跨 `update`/upsert 不变,访问统计与关系边始终有效 | `tests/stable_rowid.rs` | Planned |
| FC-MODEL-INV-024 | INV | **I24**:`update` 新版本对读者原子可见,旧版本立即遮蔽 | `tests/update_atomic.rs` | Planned |
| FC-MODEL-INV-025 | INV | **I25**:悬挂边不可见;删除一端后边立即失效,compaction 后物理清除 | `tests/relation_cascade.rs` | Planned |
| FC-MODEL-INV-026 | INV | **I26**:`as_of(t)` = 事务时间 ≤ t 的最新可见版本组成的一致快照,不随后续写入/compaction 变化;历史版本默认永久保留(`history_horizon=None`) | `tests/as_of_consistency.rs` | Planned |
| FC-MODEL-POST-001 | POST | `relate` / `relate_with_meta` 以 `(from,to,kind)` 幂等 upsert(后者同时覆盖 metadata) | `tests/relate_idempotent.rs` | Planned |
| FC-MODEL-POST-002 | POST | `consolidate` 幂等:已沉淀簇跳过;`keep_sources=true` 不删除来源 | `tests/consolidate_idempotent.rs` | Planned |
| FC-MODEL-POST-003 | POST | `supersede` 后旧版本 `valid_to` = 新版本 `valid_from`(历史可见性受 compaction 回收约束,I26) | `tests/supersede_validity.rs` | Planned |
| FC-MODEL-POST-004 | POST | **版本链保留**:每个 RowId 的最新版本与 `history_horizon` 内的历史版本被保留;仅超期版本被回收,`as_of` 在窗口内可读 | `tests/history_retention.rs` | Planned |
| FC-MODEL-POST-005 | POST | `predecessors(to, kinds)` 只返回 `edge.to == to` 且 `edge.kind ∈ kinds`、两端存活的边;`RelationIndex::Outgoing` 与 `Both` 结果一致(反向索引只加速、不改语义) | `tests/relation_direction.rs` | Planned |
| FC-MODEL-STA-001 | STA | 记忆版本:`Active(seqno_max,当前可见) → Shadowed(被遮蔽,仅 `as_of` 可见) → Reclaimed(超出 `history_horizon` 后物理回收)`;`Shadowed` 不可作为当前查询结果 | `tests/version_state.rs` | Planned |

---

## 5. 生命周期(life)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-LIFE-INV-008 | INV | **I8**:活跃段数 ≤ `(T−1)·log_r(N/B)+c`;WAL ≤ `wal_bytes` | `tests/long_run.rs` | Planned |
| FC-LIFE-INV-009 | INV | **I9**:逻辑过期/墓碑记录在常规读路径永不返回(仅 `iter_with(..., true)` 审计入口可见);物理回收仅在 compaction 提交后 | `tests/expiry_invisible.rs` | Planned |
| FC-LIFE-INV-010 | INV | **I10**:compaction 崩溃 → 恢复后 = 提交前状态(孤儿段清理) | `tests/compaction_crash.rs` | Planned |
| FC-LIFE-INV-011 | INV | **I11**:备份目录独立 `open` + `check` 通过 | `tests/backup_restore.rs` | Planned |
| FC-LIFE-INV-017 | INV | **I17**:`SnapshotHandle` 视图一致,后台 compaction 不影响 | `tests/snapshot_concurrent.rs` | Planned |
| FC-LIFE-INV-023 | INV | **I23**:自动遗忘默认关闭;删除可审计(墓碑在 `history_horizon` 内保留,默认永久,经 `iter_with(..., true)` 可见),绝不静默 | `tests/retain_audit.rs` | Planned |
| FC-LIFE-POST-001 | POST | `retain` 返回 `forgotten` 与 `sampled_ids` 与实际墓碑一致 | `tests/retain_report.rs` | Planned |

---

## 6. 量化与门面(quant)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-QUANT-INV-012 | INV | **I12**:量化模式 `Hit.score` = f32 精排分 | `tests/quant_score.rs` | Planned |
| FC-QUANT-INV-013 | INV | **I13**:召回不达标自动回退 f32,`stats()` 可见 | `tests/quant_fallback.rs` | Planned |
| FC-QUANT-INV-014 | INV | **I14**:async 与 sync API 等价(共享同一写锁) | `tests/async_equiv.rs` | Planned |
| FC-QUANT-ERR-001 | ERR | 未开 `quant-f16` 时 `VectorFormat::F16` 构造期返回 `Invalid`,不静默降级 | `tests/f16_disabled.rs` | Planned |

---

## 7. 安全与部署(security/deploy)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-SEC-INV-028 | INV | **I28**:开启加密后磁盘无明文记录字段;认证失败 → `Corrupted` | `tests/encryption_no_plaintext.rs` | Planned |
| FC-SEC-POST-001 | POST | 密钥轮换后新旧密钥均可读,迁移完成旧 key 退役 | `tests/key_rotation.rs` | Planned |
| FC-DEPLOY-INV-029 | INV | **I29**:只读实例看到的始终是某已提交 MANIFEST 版本的完整视图 | `tests/readonly_consistency.rs` | Planned |
| FC-DEPLOY-INV-030 | INV | **I30**:`Observer` 回调不改变引擎行为;回调 panic 被隔离 | `tests/observer_isolation.rs` | Planned |
| FC-DEPLOY-STA-001 | STA | 只读视图切换:`V_n → V_{n+1}` 原子;不存在中间态 | `tests/readonly_switch.rs` | Planned |

---

## 8. 边界与极值(全局)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-GLOBAL-PRE-001 | PRE | 向量维度 ∈ [1, 65536];长度 ≠ 建库维度 → `DimensionMismatch` | `tests/dimension_bounds.rs` | Planned |
| FC-GLOBAL-PRE-002 | PRE | 任一分量 `NaN`/`±Inf` → `Invalid`,库内不被污染 | `tests/nonfinite_reject.rs` | Planned |
| FC-GLOBAL-PRE-003 | PRE | key ≤ 1024B、text ≤ 1MiB、meta ≤ 64KiB、深度 ≤ 32 → 否则 `TooLarge` | `tests/limits.rs` | Planned |
| FC-GLOBAL-PRE-004 | PRE | `importance`/`confidence` 越界钳制到 [0,1];`top_k`/`ef` ≤ 4096 | `tests/clamp_bounds.rs` | Planned |
| FC-GLOBAL-ERR-001 | ERR | 库绝不 panic;`filter!` 字面量与 async `spawn_blocking` 为文档化例外 | `tests/no_panic.rs` | Planned |
| FC-GLOBAL-PRE-005 | PRE | 时钟回拨经单调水位钳制;记录不会因回拨早消失 | `tests/clock_rewind.rs` | Planned |

---

## 9. 追溯规则(FSVDD 强制)

1. **无孤儿实现**:任何新增业务逻辑必须在本矩阵登记至少一条约束;
2. **无失效契约**:本矩阵条目若与代码不符,以"先改契约、再改测试、再改代码"为准;
3. **无孤立测试**:每个测试文件头部注释必须引用其覆盖的 `FC-*` 编号;
4. **100% 映射**:本矩阵条目数与测试套件条目数一一对应,CI 校验
   (`xtask check-contracts`,见 [14 §8](../design/14-testing.md));
5. **豁免**:纯文档改动不新增契约;但涉及磁盘格式/API 语义的文档改动必须先更新本矩阵。
