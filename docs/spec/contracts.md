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

> 契约与格式的版本记录:`格式版本`表登记磁盘布局的破坏性演进,`语义里程碑`表登记各批次落地
> 的形式化语义,`验收环境要求`表登记需要专用 runner 的手动门槛。项目未发布,格式版本按
> `FORMAT_VERSION` 精确匹配,不留旧格式读取分支(见 [04 §12](../design/04-l2-persist.md))。

#### 格式版本

| 版本 | 批次 | 布局变更 |
|---|---|---|
| `0x0006` | 11 存储安全 | msec 记录体新增 `flags2` 字节(text/meta/provenance 压缩位);未压缩时字段编码与上一版一致,仅多该字节 |
| `0x0005` | L6 量化 | vsec 新增 qvec 区(量化码流,`FC-QUANT-POST-002`);`quant` 字节 0=F32 / 1=i8 / 2=f16 |
| `0x0004` | L5 生命周期 | msec 关系区新增全量/增量标志(`FLAG_FULL`);增量段按 upsert 应用(`FC-MODEL-POST-007`) |
| `0x0003` | L5 生命周期 | msec `zmap` 区尾新增 `ttl_map`(块级 TTL 剪枝,`FC-LIFE-CPLX-001`) |
| `0x0002` | L4 检索 | msec 新增 `field_dict`/`zmap`/`bloom`/`inverted` 四区(`FC-PERSIST-POST-008`) |
| `0x0001` | L0–L3 | 初版:vsec/msec/MANIFEST/WAL 文件布局与 hidx HID1 图格式 |

#### 语义里程碑

| 批次 | 交付内容 |
|---|---|
| L0 原语 | 类型/错误/ID、SIMD 距离、TopK 堆、varint、meta、BitSet(`FC-CORE-*`) |
| L1 内存引擎 | 公开 API 冻结、写事务(失败零部分写入)、墓碑/版本链、去重四模式、MVCC 快照读(`FC-MEM-*`) |
| L2 持久层 | WAL-before-visible 与组提交、全量/增量段、MANIFEST 原子提交、崩溃恢复、`FsyncHook` 崩溃注入、`backup_to`(`FC-PERSIST-*`) |
| L3 索引层 | 自研 HNSW(批内并行 + `BuildPrecision::Hybrid`)、hidx 持久化与惰性载入、过滤三档、mmap(`FC-INDEX-*`) |
| L4 检索层 | 过滤 DSL、zone map/bloom 计划器、BM25、RRF/加权融合、结果级去重(`FC-QUERY-*`) |
| L5 生命周期 | 指数遗忘曲线、增量段、size-tiered compaction、WAL 轮转/Checkpoint、后台维护、命名空间注销、快照/备份/fsck、`RelationIndex::Both`(`FC-LIFE-*`/`FC-MODEL-*`) |
| L6 打磨 | i8/f16 量化副本、两阶段精排与建段抽样回退、async 门面(`FC-QUANT-*`) |
| 09/10 记忆能力 | 双时态 `as_of`/`supersede`、关系图与自定义关系注册表、候选放大/偏置路由、联想扩展、反馈闭环(`FC-MODEL-*`/`FC-SCORE-*`) |
| 11 存储安全 | AES-256-GCM 信封加密与密钥轮换、LZ4 风格与 zstd 压缩(`FC-SEC-*`) |
| 12 部署形态 | `Storage` 后端抽象、多进程只读共享与 `reload`、`Observer` 事件(`FC-DEPLOY-*`) |
| 性能批次 | 视图级计划/段位图缓存、段间并行搜索、AVX-512/AVX2 内核、批内并行建图、惰性段驻留、`ChunkedVec`/`ShardedMap`、冷启动常数优化(见各 `FC-*-CPLX-*`) |
| 工程化 | CI fast/middle 两档、`contract_traceability` 双向追溯门禁、`mutants.toml`、fuzz 五目标与长跑脚本、库本体配置显式化(不读环境变量,`FC-GLOBAL-INV-001`) |
| 能力收尾 | WAL 单帧限额写路径强制(`FC-PERSIST-ERR-013`)、残余谓词选择性重排(`FC-QUERY-POST-009`)、`io_budget` 单轮输入字节预算(`FC-LIFE-POST-011`) |

#### 验收环境要求

| 项 | 说明 |
|---|---|
| 正式规模性能门槛 | `FC-PERSIST-POST-013`(冷启动)与 `FC-GLOBAL-CPLX-001`(无隐藏复杂度)的正式规模断言为 1M×1536,需 ≥16GB 专用 runner 手动执行(`MNEME_HEAVY=1`;回归建议 100_000×128);用例与断言已就位,任意规模下的正确性断言随 heavy 档运行 |
| GPU/CAGRA 建库档 | ≈50k+/s 登记为可选 GPU 后端的远期目标,不参与 CPU 门槛,不属当前承诺范围 |

### 0.2 错误分类矩阵(Error Taxonomy)

> `MnemeError`(`src/core/error.rs`,标注 `#[non_exhaustive]`)是整库唯一对外错误类型。
> **每个偏离形式化约束的语义类都必须有专属变体**——禁止用一个泛化的 `Invalid(&'static str)`
> 承载多种互不相同的失败(FSVDD §2.5)。下表是错误语义的唯一真实数据源。

| 变体 | 触发条件 | 对应 FC | 备注 |
|---|---|---|---|
| `Io` | 底层 I/O 失败 | — | `#[from] std::io::Error` |
| `Corrupted { segment, reason }` | CRC/魔数不符等数据损坏(msec `delta` 区畸形见 FC-PERSIST-ERR-011;vsec qvec 区畸形见 FC-QUANT-ERR-003;恢复期水位溢出见 FC-PERSIST-ERR-012) | FC-CORE-ERR-001、FC-INDEX-ERR-001、FC-INDEX-ERR-002、FC-PERSIST-ERR-011、FC-PERSIST-ERR-012、FC-QUANT-ERR-003 | `segment=None` 表示文件级损坏 |
| `DimensionMismatch { expected, got }` | 向量长度 ≠ 建库维度(写/查) | FC-GLOBAL-PRE-001、FC-MEM-PRE-001/004 | |
| `MetricMismatch { existing, requested }` | 打开时度量与库中记录不符 | — | 持久库打开路径使用 |
| `KeyMismatch { expected, got }` | `supersede` 的新记录自带 key 与目标 key 冲突(信念修订须沿用同一 key) | FC-MODEL-POST-003 | 新记录省略 key 时继承目标 key |
| `KeyNotFound(Key)` | 键不存在 | — | 公开错误面的预留变体:查询不存在的键经 `get`/`exists` 返回 `Ok(None)`/`false`,任何 API 均不产生本变体 |
| `DuplicateKey(Key)` | `InsertMode::RejectDuplicate` 命中,或写版本携带的 key 已被另一可见记录占用(key 迁移冲突) | FC-MEM-POST-001、FC-MEM-POST-007 | |
| `FilterParse(String)` | 过滤 DSL 语法错误(带位置) | FC-QUERY-ERR-001 | L4 已使用 |
| `Busy(&'static str)` | 独占锁被占/备份中 | — | 保留 |
| `TooLarge { field, limit, got }` | 字段载荷超限额(key/text/meta 字节) | FC-GLOBAL-PRE-003、FC-MEM-PRE-002 | |
| `UnsupportedVersion { file, found, max }` | 文件格式版本与当前定义不一致(精确匹配) | FC-PERSIST-ERR-002、FC-INDEX-ERR-001 | 段/MANIFEST/WAL 打开校验使用 |
| `Closed` | 库已关闭后经任意句柄读写 | FC-MEM-ERR-001、FC-MEM-STA-001 | 原 `Invalid("closed")` |
| `NonFinite` | 向量分量或 `importance`/`confidence`/边权/`boost` 等数值输入含 `NaN`/`±Inf`(会污染打分、遗忘公式与排序) | FC-GLOBAL-PRE-002、FC-GLOBAL-PRE-004、FC-MEM-PRE-001/003 | 原 `Invalid("向量分量必须是有限值")` |
| `LimitExceeded { field, limit, got }` | 参数越上限(维度、`top_k`、`ef`、`ef_search`、HNSW 度数) | FC-CORE-PRE-001、FC-GLOBAL-PRE-004、FC-MEM-PRE-003、FC-INDEX-PRE-001 | 原 `Invalid("top_k 超过上限")` 等 |
| `MetaTooDeep { limit, got }` | metadata 嵌套深度超限 | FC-GLOBAL-PRE-003、FC-MEM-PRE-002 | 原 `Invalid("metadata 嵌套过深")` |
| `Config { reason }` | 建库/查询配置非法(缺维度、无查询通道、MMR `lambda` 非有限值、`Fusion` 未同时启用双通道、`Weighted.alpha` 越界或非有限)、策略参数含非有限值(`min_importance`/`access_weight`/`threshold`/`dedup_threshold`)或越界(`dedup_threshold`/`threshold` ∉ [0,1])或非法(`max_cluster = 0`)、HNSW 参数域非法(`m < 2`/`m0 < m`/`ef_construction = 0`/`ef_search = 0`/过滤阈值越界或 `brute > post`)、命名空间路径深度超 `Limits.ns_depth` 或含非法字符(首次写入时,`namespace()` 本身不返回 `Result`) | FC-MEM-STA-001、FC-MEM-PRE-003、FC-MODEL-POST-006、FC-LIFE-POST-002、FC-LIFE-POST-005、FC-GLOBAL-PRE-004、FC-INDEX-PRE-001、FC-MEM-ERR-002 | 原 `Invalid("新建内存库必须指定维度")` 等 |
| `Unsupported { feature }` | 与 feature 门控或库形态不符的能力以结构化错误返回,绝不静默降级:只读模式写入、纯内存库 `backup_to`/量化、未开启的 feature 门控(见 FC-MEM-ERR-002、FC-QUANT-ERR-001/002) | FC-MEM-ERR-002、FC-PERSIST-ERR-003、FC-QUANT-ERR-001、FC-QUANT-ERR-002 | 只读写、**纯内存库** backup、量化 feature/纯内存限制 |
| `Inconsistent { reason }` | 内部不变量被破坏 | FC-MEM-INV-004 | 原 `Invalid("去重命中但记录不可见")` |
| `IdExhausted { kind }` | `RowId`/`NsId`/`SeqNo` 或段号/MANIFEST 版本的整型表示空间耗尽(恢复出近上限水位后再写入/提交),绝不回绕复用(FC-PERSIST-ERR-012) | FC-PERSIST-INV-020、FC-PERSIST-ERR-012 | 理论不可达,防线保留 |

---

## 1. 原语层(core / L0)

> 无 I/O、无全局状态、无锁的纯类型与纯函数;`unsafe` 仅在 `simd/` 的 arch 内联
> 与 L2 `persist/source/` 的 `MmapSource` 两处(均附 `// SAFETY:`)。
> 层边界契约见 [02 §9](../design/02-l0-core.md)。

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-CORE-PRE-001 | PRE | `Dimension::new(d)` 仅接受 `d ∈ [1, 65536]`;越界返回 `LimitExceeded`,内部不再使用裸整数 | `tests/core_contracts.rs::dimension_bounds` | Passed |
| FC-CORE-POST-001 | POST | `Metric::score` 严格映射:Dot = a·b;Cosine = a·b / √(a_norm·b_norm);Euclidean = a_norm + b_norm − 2·a·b(其中 norm 为**范数平方**) | `tests/core_contracts.rs::metric_score_mapping` | Passed |
| FC-CORE-POST-002 | POST | `Metric::better(x,y)` 给出统一方向:Cosine/Dot 分数越大越优,Euclidean 越小越优;TopK/归并/排序一律经此比较;分数不可比(`NaN`)时由 `Metric` 侧全序(`score_order`,`NaN` 恒排最后、`±0` 由 `total_cmp` 区分)定序 | `tests/core_contracts.rs::metric_better_direction`、`src/core/metric.rs::score_order_is_total_for_nan_and_signed_zero` | Passed |
| FC-CORE-POST-003 | POST | `Metric::needs_norm()` 仅 `Dot` 返回 `false`;`Cosine`/`Euclidean` 返回 `true` | `tests/core_contracts.rs::metric_needs_norm` | Passed |
| FC-CORE-POST-004 | POST | `TopK<T: Ord>` 恒保留按 `better` 方向最优的 k 个;分数不可比(`NaN`)与精排共用同一全序(NaN 恒排最后),同分按载荷升序稳定;`merge` 结果 ≡ 顺序 `push` 全部元素;`into_sorted_vec` 最优在前 | `tests/core_contracts.rs::topk_equivalence`、`src/core/heap/tests.rs::topk_orders_nan_consistently` | Passed |
| FC-CORE-POST-005 | POST | varint 编解码往返:`decode_u64(encode_u64(x)) == (x, len)`、`decode_u32(encode_u32(x)) == (x, len)` 且编码为最小编码(无多余 continuation 字节) | `tests/core_contracts.rs::varint_roundtrip_minimal` | Passed |
| FC-CORE-POST-006 | POST | `meta::get_path` 按 `.` 分段遍历对象,缺失/类型不符返回 `None`;`as_f64/as_i64/as_bool/as_str/as_ts` 仅匹配对应 JSON 类型 | `tests/core_contracts.rs::meta_accessors` | Passed |
| FC-CORE-POST-007 | POST | `RelationKind` 内置常量 `DERIVED_FROM=0`、`SUPPORTS=1`、`CONTRADICTS=2`、`RELATED=3`;自定义编号从 16 起 | `tests/core_contracts.rs::relation_kind_builtins` | Passed |
| FC-CORE-POST-008 | POST | `tokenize` 分词口径(设计 06 §3.5):按 Unicode 空白分段并去首尾非字母数字;非 CJK 段整词小写化;连续 CJK 段切 bigram(单字保留);`stopwords_enabled` 控制内置停用词过滤;空白/纯标点输入 → 空 `Vec`,输出保序 | `src/core/text.rs::latin_words_are_lowercased_and_punctuation_trimmed`、`src/core/text.rs::cjk_runs_become_bigrams`、`src/core/text.rs::single_cjk_char_is_kept`、`src/core/text.rs::mixed_script_splits_at_script_boundary`、`src/core/text.rs::stopwords_are_filtered_only_when_enabled`、`src/core/text.rs::whitespace_only_text_yields_no_tokens`、`src/core/text.rs::punctuation_only_text_yields_no_tokens` | Passed |
| FC-CORE-POST-009 | POST | `ChunkedVec`:固定块(1024)分块的追加型向量,`push`/`get`/`get_mut`/`Index`/`IndexMut`/`iter`/`len` 语义与平铺 `Vec<T>` 全等(含 0、块边界、跨多块);克隆只复制块句柄;`push` 与 `get_mut` 走**块级 COW**,任何修改绝不泄漏给已克隆视图;空间 $O(len)$(块表 $O(len/\text{CHUNK})$) | `src/core/chunked.rs::matches_vec_semantics_across_chunk_boundaries`、`src/core/chunked.rs::clone_shares_chunks_and_push_is_copy_on_write`、`src/core/chunked.rs::get_mut_is_copy_on_write_per_chunk`、`src/core/chunked.rs::reserve_keeps_semantics` | Passed |
| FC-CORE-POST-010 | POST | `ShardedMap`:固定 256 分片的行号哈希表,`get`/`get_mut`/`get_or_insert_default`/`insert`/`remove`/`iter`/`values`/`len` 语义与 `HashMap<RowId, V>` 全等(迭代顺序不保证);克隆只复制分片句柄;写操作走**分片级 COW**,任何修改绝不泄漏给已克隆视图;`reserve` 仅应在表唯一持有时调用(共享时等于整表拷贝) | `src/core/sharded.rs::matches_hashmap_semantics_across_shards`、`src/core/sharded.rs::clone_shares_shards_and_writes_are_copy_on_write`、`src/core/sharded.rs::get_mut_and_get_or_insert_are_copy_on_write` | Passed |
| FC-CORE-INV-001 | INV | `simd::dot(a,b)` 与标量参考实现等价(容差内),覆盖长度非 LANE 倍数与空切片 | `tests/core_contracts.rs::dot_matches_scalar_reference` | Passed |
| FC-CORE-INV-002 | INV | L0 公开 API 在其定义域内对任意输入不 panic、无 UB(畸形 varint、空向量、越界维度等);`simd::dot` / `Metric::score` 要求两切片等长,长度不等属调用方违约(debug 断言,release 按较短者计算) | `tests/core_contracts.rs::no_panic` | Passed |
| FC-CORE-ERR-001 | ERR | varint 解码遇截断或超长(> 10 字节)返回结构化 `Corrupted`,绝不 panic、绝不静默跳过 | `tests/core_contracts.rs::varint_malformed` | Passed |
| FC-CORE-ERR-002 | ERR | 余弦分母 `√(a_norm·b_norm) = ‖a‖·‖b‖ < ε`(零向量,ε=1e-12)时返回 `0`,绝不返回 `NaN` | `tests/core_contracts.rs::cosine_zero_vector` | Passed |

---

## 1.5 内存引擎(memory / L1)

> 全内存实现,公开 API 在此冻结(见 [03](../design/03-l1-memory.md) §8)。
> 并发模型 = 单写者 `Mutex<WriterState>` + 读者克隆 `Arc<ReaderView>` 后无锁扫描;
> 同快照内排序全等。`RecordRef` 以 `Arc` 持有记录数据并暴露访问器方法
> (而非借用字段),以在安全 Rust 下满足 `get() -> RecordRef<'_>` 的签名。

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-MEM-PRE-001 | PRE | `insert` 向量长度 ≠ 建库维度 → `DimensionMismatch`;任一分量 `NaN`/`±Inf` → `NonFinite`,库内不被污染(FC-GLOBAL-PRE-001/002) | `tests/memory_contracts.rs::insert_rejects_dimension_and_non_finite` | Passed |
| FC-MEM-PRE-002 | PRE | key ≤ `Limits.key_bytes`、text ≤ `text_bytes`、meta ≤ `meta_bytes` 且深度 ≤ `meta_depth`,否则 `TooLarge`/`MetaTooDeep`(FC-GLOBAL-PRE-003);**`insert`/upsert/supersede/merge 与 `update` 补丁全部同口径**,更新失败的记录保持上一版本原样(零部分写入) | `tests/memory_contracts.rs::write_limits_reject_too_large`、`tests/memory_contracts.rs::write_meta_limits_reject_too_large`、`tests/memory_contracts.rs::write_limits_boundary_three_point`、`tests/memory_contracts.rs::update_enforces_write_limits`、`tests/memory_contracts.rs::supersede_and_merge_enforce_write_limits` | Passed |
| FC-MEM-PRE-003 | PRE | `top_k`/`ef` 超过 `Limits.top_k_max`/`ef_max` → `LimitExceeded`;综合打分各因子(相似度归一、新鲜度、访问频次)与 MMR `lambda` 钳制到 `[0,1]`(权重本身无 `[0,1]` 定义域),**MMR `lambda` 含非有限值(NaN/±Inf)时钳制对 NaN 失效、会静默退化为固定取首项,故于 `execute()` 入口返回 `Config`,绝不静默**;`importance`/`confidence` 写入期越界钳制到 `[0,1]`,含非有限值(NaN)→ `NonFinite`,绝不入库(FC-GLOBAL-PRE-004) | `tests/memory_contracts.rs::importance_and_confidence_clamped`、`tests/memory_contracts.rs::importance_confidence_boundary_three_point`、`tests/query_contracts.rs::search_limits_reject_top_k_and_ef`、`tests/query_contracts.rs::scoring_composite_factors_clamped` | Passed |
| FC-MEM-PRE-004 | PRE | 检索查询向量长度 ≠ 建库维度 → `DimensionMismatch`(与写入校验同口径,设计 03 §2.2) | `tests/query_contracts.rs::search_rejects_dimension_mismatch` | Passed |
| FC-MEM-POST-001 | POST | 每次成功写入分配全库单调 `SeqNo`;同 key `Upsert` 保留既有 `RowId` 并写新物理版本;`RejectDuplicate` 仅当同 key 存在**可见**记录(未墓碑且未逻辑过期)时 → `DuplicateKey`(I22),墓碑/逻辑过期记录视为不存在并复用既有 `RowId`(与 `Dedup::Reject`/读路径同口径,FC-LIFE-INV-009) | `tests/memory_contracts.rs::upsert_keeps_rowid_and_rejects_duplicate`、`tests/memory_contracts.rs::reject_duplicate_ignores_expired_and_deleted` | Passed |
| FC-MEM-POST-002 | POST | `insert_batch` 整批原子(I15):任一条维度/数值/限额校验失败 → 整批拒绝、零部分写入;预校验后逐条求值仍可能失败(`Dedup::Merge` 回调产物超限、槽位容量溢出)时回滚到批前状态,不残留任何版本;**所有写操作经写事务(`Table::write_tx`)执行,失败回滚到操作前状态且不发布读视图,副作用(含命名空间登记)一并回滚**;`RejectDuplicate`/`Dedup::Reject` 为逐条结果,命中处返回 `Duplicate`,其余照常写入 | `tests/memory_contracts.rs::insert_batch_is_atomic_with_per_row_duplicates`、`tests/memory_contracts.rs::failed_writes_do_not_register_namespace` | Passed |
| FC-MEM-POST-003 | POST | `update` 保留 `RowId`,写入新物理版本(新 seqno),对读者原子可见;旧版本立即遮蔽(I24) | `tests/memory_contracts.rs::update_is_atomically_visible` | Passed |
| FC-MEM-POST-004 | POST | `delete`/`forget` 打墓碑;墓碑与逻辑过期记录在常规读路径(`get`/`search`/`iter`/`count`)永不返回,仅 `iter_with(_, true)` 可见(I9) | `tests/memory_contracts.rs::delete_hides_records_from_reads` | Passed |
| FC-MEM-POST-005 | POST | `get_many`/`get_many_by_rowid` 返回顺序与输入一一对应;`count` 与 `search`/`iter` 过滤语义一致(预过滤,墓碑不计) | `tests/memory_contracts.rs::batch_point_reads_preserve_order_and_count`、`tests/memory_contracts.rs::batch_rowid_reads_preserve_order`、`tests/query_contracts.rs::filter_matches_bruteforce_prop` | Passed |
| FC-MEM-POST-006 | POST | TTL 相对时长经 `Clock` 即刻换算为绝对 `expires_at`(Unix 毫秒);`importance` 缺省 0.5、`confidence` 缺省 1.0,均钳制到 `[0,1]` | `tests/memory_contracts.rs::ttl_is_converted_to_expires_at`、`tests/memory_contracts.rs::importance_and_confidence_clamped` | Passed |
| FC-MEM-POST-007 | POST | 写入期去重(FC-INDEX-POST-004):`Reject` → `Duplicate{existing,score}`;`Replace` → 墓碑旧行、新 `RowId`(即使带同 key 也不复用旧行);`Merge` → 就地更新并保留旧 `RowId`,回调 `None` 等价 `KeepBoth`;合并/替换产物若改变 key,`key_index` 随新版本迁移(旧 key 映射移除,不悬挂);**若产物 key 已被另一可见记录占用(未墓碑、未逻辑过期)→ `DuplicateKey` 并整体回滚,绝不静默覆盖他人 `key_index`** | `tests/memory_contracts.rs::dedup_reject_replace_and_merge`、`tests/memory_contracts.rs::dedup_replace_with_key_gets_new_rowid`、`tests/memory_contracts.rs::dedup_merge_key_change_migrates_index`、`tests/memory_contracts.rs::key_migration_rejects_live_key_conflict` | Passed |
| FC-MEM-POST-008 | POST | `check()` 对内部一致的健康库(含已删除/已逻辑过期记录)返回 `ok=true`;仅当 key 索引指向不存在的物理版本,或最新版本 `ns_id`/`key` 与索引不符时报告不一致——墓碑/逻辑过期记录不算不一致 | `tests/life_contracts.rs::check_reports_healthy_after_delete`、`tests/life_contracts.rs::check_reports_healthy_after_expiry` | Passed |
| FC-MEM-POST-009 | POST | `touch`/`touch_by_rowid` 仅对可见记录(未墓碑、未逻辑过期)生效返回 `true`,否则返回 `false` 且不更新访问统计与 `importance`(与 `feedback`/读路径同口径,I9) | `tests/memory_contracts.rs::touch_only_affects_visible_records` | Passed |
| FC-MEM-INV-001 | INV | `RowId` 跨 `update`/upsert 不变,访问统计与关系边始终指向同一逻辑记忆(I22) | `tests/memory_contracts.rs::upsert_keeps_rowid_and_rejects_duplicate`、`tests/memory_contracts.rs::seqno_and_rowid_stable_prop` | Passed |
| FC-MEM-INV-002 | INV | `SeqNo` 全库单调递增、永不复用;快照以 seqno 水位界定可见版本 | `tests/memory_contracts.rs::upsert_keeps_rowid_and_rejects_duplicate`、`tests/memory_contracts.rs::seqno_and_rowid_stable_prop` | Passed |
| FC-MEM-INV-003 | INV | **排序全等性**(FC-INDEX-POST-003):同一快照内任意两次同参数 `execute()` 结果逐位相同,同分按 `RowId` 升序 | `tests/query_contracts.rs::search_order_is_total_and_stable` | Passed |
| FC-MEM-INV-004 | INV | `SlotId` 随物理版本单调递增、永不复用;槽位容量溢出(`u32::MAX`)返回结构化错误,绝不静默饱和 | `src/memory/table/state/tests.rs::slot_id_for_rejects_overflow` | Passed |
| FC-MEM-STA-001 | STA | 库生命周期五元组 `M=(States={Open,Closed}, Events={Close}, δ(Open,Close)=Closed, δ(Closed,Close)=Closed, s0=Open, F={Closed})`;`Closed` 后经任意句柄读写 → `Closed`;重复 `close` 幂等返回 `Ok` | `tests/life_contracts.rs::database_lifecycle_open_closed` | Passed |
| FC-MEM-ERR-001 | ERR | `close` 后(经任意克隆句柄)读写返回 `Closed`;重复 `close` 返回 `Ok`(幂等) | `tests/life_contracts.rs::closed_database_rejects_operations` | Passed |
| FC-MEM-ERR-002 | ERR | 与 feature 门控或库形态不符的能力以结构化错误返回、绝不静默:**纯内存库** `backup_to` → `Unsupported`(持久库 `backup_to` 见 `FC-PERSIST-POST-004`);`Fusion` 未同时启用向量与文本通道 → `Config`(设置即拒绝,不静默忽略);`Scoring::bias_routing` 见 `FC-SCORE-POST-007`;新建持久库缺维度 → `Config`(`open`/`path` 见 `FC-PERSIST-ERR-004`) | `tests/life_contracts.rs::deferred_features_return_structured_errors`、`tests/l4_contracts.rs::hybrid_fusion_and_validation`、`tests/query_contracts.rs::bias_routing_only_changes_visit_order` | Passed |

---

## 2. 持久层(persist)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-PERSIST-INV-001 | INV | **I1**:已确认写入不半写;未确认写入重启后要么完整可见要么不存在;写事务 append/sync 失败时截断半写帧(失败写绝不持久、不遮挡后续已确认写) | `tests/persist_contracts.rs::reopen_after_close_recovers_records`、`tests/persist_contracts.rs::reopen_after_drop_recovers_from_wal`、`tests/persist_contracts.rs::injected_wal_failure_keeps_confirmed_prefix`、`tests/persist_contracts.rs::injected_fsync_failure_rolls_back_frames` | Passed |
| FC-PERSIST-INV-002 | INV | **I2**:任意 bit 损坏可检出或拒绝启动,绝不静默返回错误数据 | `src/persist/vsec/tests.rs::vsec_detects_header_corruption`、`src/persist/vsec/tests.rs::vsec_detects_payload_corruption`、`src/persist/manifest/tests.rs::manifest_detects_header_corruption`、`src/persist/manifest/tests.rs::manifest_detects_payload_corruption`、`src/persist/wal/mod.rs::wal_bad_crc_stops_replay`、`tests/persist_contracts.rs::verify_on_open_detects_payload_corruption`、`tests/persist_contracts.rs::check_detects_corrupt_segment` | Passed |
| FC-PERSIST-INV-003 | INV | **I3**:活跃段集合 = 某 MANIFEST 版本所列集合 | `tests/persist_contracts.rs::flush_checkpoints_after_materialize` | Passed |
| FC-PERSIST-INV-004 | INV | **I4**:WAL 总量 ≤ **硬上限** `12 × wal_bytes`;`wal_bytes` 为软阈值——达到后若未落盘行数不足「并行度 × 块行数」则继续累积(避免大维度下每次只物化一个块、单核串行),行数达标或 WAL 达硬上限即触发增量段 flush;段文件只增不改 | `tests/persist_contracts.rs::wal_capacity_triggers_incremental_flush`、`tests/persist_contracts.rs::wal_soft_threshold_waits_for_parallel_rows`、`tests/persist_contracts.rs::committed_segment_is_write_once` | Passed |
| FC-PERSIST-INV-005 | INV | **I1(撕裂尾部)**:可写重开时物理截断 WAL 撕裂尾部(有效长度 = 头部 + 完整帧),使截断之后的追加写入不被残尾永久屏蔽;有效前缀不受影响;WAL 短于文件头(Checkpoint 重置中途崩溃)视为撕裂头并重建,不拒绝打开;重置/回滚写头与 fsync 经 `FsyncHook` 可注入,重置写头失败时重建、重建仍失败则停用句柄(`Io`),绝不向状态可疑的文件继续追加 | `tests/persist_contracts.rs::torn_wal_tail_truncated_on_reopen`、`tests/persist_contracts.rs::short_wal_header_is_recreated_on_open`、`src/persist/store/wal_writer/tests.rs::failed_reset_poisons_writer` | Passed |
| FC-PERSIST-INV-006 | INV | **I1(提交点)**:WAL 帧成功 fsync 即为提交点;其后后台 flush 失败不回滚已提交写,绝不出现「返回 `Err` 但重启后可见」的矛盾(flush 失败由下次写重试,`stats().wal_bytes` 可观测) | `tests/persist_contracts.rs::flush_failure_does_not_lose_committed_write` | Passed |
| FC-PERSIST-INV-019 | INV | **I19(记录级)**:`delete`/`update` 返回 `Ok` 后,崩溃 + WAL 截断仍生效,删除永不复活(`touch`/`relate` 的 WAL 帧见 FC-PERSIST-POST-005) | `tests/persist_contracts.rs::delete_survives_flush_and_reopen`、`tests/persist_contracts.rs::crash_after_delete_does_not_resurrect`、`tests/persist_contracts.rs::update_survives_reopen` | Passed |
| FC-PERSIST-INV-020 | INV | **I20**:`path↔NsId`、`next_ns_id`、`next_rowid` 可由 MANIFEST+WAL 重建,ID 永不复用;恢复期与 MANIFEST 提交期推进水位(`rowid`/`ns_id`/`next_segment_id`/`manifest_version`)一律 `checked_add`,WAL/段声称 `rowid = u64::MAX` 或 `ns_id = u32::MAX`(无法再分配下一号)→ `Corrupted`(FC-PERSIST-ERR-012);`RowId`/`NsId`/`SeqNo`/段号/MANIFEST 版本分配器同为 checked,空间耗尽 → `IdExhausted`,绝不回绕、复用或 panic | `tests/persist_contracts.rs::namespace_and_rowid_survive_reopen`、`src/persist/recover/wal_replay.rs::wal_replay_rejects_max_rowid`、`src/persist/recover/wal_replay.rs::wal_replay_rejects_max_ns_id`、`src/memory/table/state/tests.rs::id_allocators_reject_exhaustion` | Passed |
| FC-PERSIST-INV-021 | INV | **惰性段驻留**:打开段只读头部与 `node_table`,向量/量化码/HNSW 邻接字节长期挂在段句柄(`Arc<SegmentHandle>`)上按需读取(缺页或按需读),与整段载入语义逐位一致——同一快照内检索、点读、暴力对照均等价;打开探测只读 4 字节信封前缀,不得整读段文件;段文件 write-once 且经打开的描述符/映射读取,compaction 把旧段移入 `trash/`(或下次打开 `purge` 物理删除)只影响目录项,句柄存活期内数据仍可读(POSIX unlink/rename 语义);惰性读取遇句柄区间越界绝不静默返回错误向量,构造期即以结构化 `Corrupted` 拒绝 | `tests/persist_contracts.rs::lazy_reopen_matches_bruteforce`、`tests/persist_contracts.rs::lazy_handles_survive_compaction`、`tests/persist_contracts.rs::segment_open_probes_prefix_without_full_read`(mmap 构建:打开不整读)、`tests/cold_start.rs::cold_open_under_one_second`(heavy:惰性驻留对读语义透明的规模验收)、`src/persist/source/tests.rs::byte_file_slice_matches_file_bytes`、`src/memory/lazy.rs::lazy_vector_decodes_once_and_matches_owned` | Passed |
| FC-PERSIST-POST-001 | POST | **I15**:`insert_batch` 整批原子:可见记录数 ∈ {0, n},无部分批 | `tests/persist_contracts.rs::batch_insert_is_atomic_across_reopen` | Passed |
| FC-PERSIST-POST-002 | POST | Checkpoint 仅当 `seqno ≤ watermark` 的覆盖条目已物化(写入增量段或 `delta` 区)时才截断/删除 WAL;重置按「先写完整头、后截断」执行(重置后文件恰为文件头长且头可解析);重置失败不阻断已提交的 flush(段/MANIFEST 已提交,旧帧均 ≤ watermark),句柄安全由 `reset` 内部重建/停用保证;WAL 轮转后按文件序回放、`seqno ≤ watermark` 的帧跳过,单批不跨文件(FC-PERSIST-POST-011) | `tests/persist_contracts.rs::flush_checkpoints_after_materialize`、`src/persist/store/wal_writer/tests.rs::reset_keeps_complete_header` | Passed |
| FC-PERSIST-POST-003 | POST | **I16**:`close()` 返回 `Ok` 后所有已确认写入持久;`Drop` 不保证 | `tests/persist_contracts.rs::reopen_after_close_recovers_records` | Passed |
| FC-PERSIST-POST-004 | POST | `backup_to` 先 flush 再复制段(vsec/msec/hidx)/MANIFEST/WAL,`current` 最后写;产物可独立 `open`(设计 16 §7) | `tests/persist_contracts.rs::backup_is_independently_openable` | Passed |
| FC-PERSIST-POST-005 | POST | `touch`/`relate`/`unrelate` 的 WAL 帧持久性:崩溃后回放 `TouchRow`/`Relate`/`Unrelate` 帧,`access_count`/访问时刻与关系边不丢失(importance 强化随版本 `Insert`)、unrelate 不复活 | `tests/persist_contracts.rs::relate_and_unrelate_survive_crash`、`tests/persist_contracts.rs::touch_boost_survives_crash`、`src/persist/wal/mod.rs::wal_touch_relate_roundtrip` | Passed |
| FC-PERSIST-POST-006 | POST | WAL `Insert`/`DeleteRow` 帧携带版本事务时间 `tx_ms`;崩溃恢复后 `as_of(t)` 历史正确(删除前时点可见、删除后不可见),不以记录体 `created_at` 或 `0` 代替 | `tests/persist_contracts.rs::as_of_history_survives_reopen` | Passed |
| FC-PERSIST-ERR-001 | ERR | 未知 WAL 帧类型 → 停止回放并报错,不静默跳过 | `src/persist/wal/mod.rs::wal_unknown_frame_type_errors` | Passed |
| FC-PERSIST-ERR-002 | ERR | 文件格式版本与当前定义不一致(高或低)→ `UnsupportedVersion`(I18);段级版本不一致即使默认非 fail-fast 也拒绝打开、绝不降级为跳过;版本在头部 CRC 校验之前判定 | `src/persist/vsec/tests.rs::vsec_rejects_version_mismatch`、`src/persist/manifest/tests.rs::manifest_rejects_version_mismatch`、`tests/persist_contracts.rs::segment_version_mismatch_is_rejected` | Passed |
| FC-PERSIST-ERR-003 | ERR | 只读模式写操作 → `Unsupported { feature: "只读模式写入" }`,绝不静默;只读打开不创建/改写 WAL(设计 04 §13);只读打开绝不改动文件系统(不建目录、不清 `trash/`),库目录不存在 → `Config` | `tests/persist_contracts.rs::read_only_rejects_writes`、`tests/persist_contracts.rs::read_only_open_does_not_create_wal`、`tests/persist_contracts.rs::read_only_open_does_not_mutate` | Passed |
| FC-PERSIST-ERR-004 | ERR | 打开时显式维度与 MANIFEST 不符 → `DimensionMismatch`,拒绝打开(设计 16 §3) | `tests/persist_contracts.rs::dimension_mismatch_rejected_on_open` | Passed |
| FC-PERSIST-ERR-005 | ERR | 目录状态不一致(`current` 存在但无合法 MANIFEST,或存在段文件却既无 MANIFEST 也无 WAL)→ `Corrupted`,绝不当作新库覆盖既有数据(设计 16 §3)。注:段文件存在但**有 WAL** 属首次 flush 崩溃,见 `FC-PERSIST-STA-003`,不返回 `Corrupted` | `tests/persist_contracts.rs::corrupt_current_without_valid_manifest_is_rejected`、`tests/persist_contracts.rs::segments_without_manifest_are_rejected`、`tests/persist_contracts.rs::manifest_fallback_accepts_noncanonical_name` | Passed |
| FC-PERSIST-ERR-006 | ERR | MANIFEST 引用的段文件缺失或为空 → `Corrupted`,绝不静默跳过而少返回数据(I2/I3);**损坏段(文件存在但解析失败或区级结构畸形)在非 fail-fast 下仅在内存跳过、文件保持原地**(绝不自动移入 `trash/`,否则 MANIFEST 引用缺失会使后续打开拒启)、可再次打开、`check()` 报告,**且 compaction 计划必须排除隔离段**(绝不把损坏段当活跃段合并清除) | `tests/persist_contracts.rs::referenced_segment_missing_is_rejected`、`tests/persist_contracts.rs::referenced_segment_empty_is_rejected`、`tests/l5_contracts.rs::skipped_corrupt_segment_keeps_library_openable`、`tests/l5_contracts.rs::corrupt_segment_is_never_compacted_away`、`tests/l5_contracts.rs::malformed_region_is_isolated_or_reported_with_segment` | Passed |
| FC-PERSIST-ERR-007 | ERR | WAL `BatchCommit` 的批内帧计数与 `batch_crc` 在回放时校验;不符 → `Corrupted`,拒绝应用半批,绝不静默(I15);未闭合批(缺 `BatchCommit`)不计入已提交长度,重开时截断,绝不吞掉其后单操作事务 | `src/persist/recover/replay.rs::replay_rejects_mismatched_batch_crc`、`src/persist/recover/replay.rs::replay_rejects_mismatched_batch_count`、`src/persist/recover/replay.rs::replay_applies_well_formed_batch`、`src/persist/recover/replay.rs::unclosed_batch_is_not_committed`、`tests/persist_contracts.rs::unclosed_batch_tail_does_not_swallow_later_writes` | Passed |
| FC-PERSIST-ERR-008 | ERR | 独占锁基于 OS 咨询锁(`std::fs::File::try_lock`):活实例持有 → `Busy`;进程崩溃/退出时内核自动释放,后续实例无需租约/接管即可获取;`Drop` 释放锁但不删除锁文件,避免不同 inode 各自加锁破坏互斥(设计 16 §3) | `src/persist/storage/tests.rs::file_lock_blocks_second_holder`、`src/persist/storage/tests.rs::file_lock_acquires_when_lock_file_exists`、`src/persist/storage/tests.rs::file_lock_file_persists_after_drop` | Passed |
| FC-PERSIST-ERR-009 | ERR | 单段恢复时的槽位重排映射(`recover::state::build_remap`,倒排载入与 hidx 载入共用,故**不要求 `hidx`**):版本行槽位越界、重复,或存在未被任何版本行引用的段内槽位(vsec/msec 行数不一致)→ `Corrupted`,绝不静默把未引用槽位映射到槽位 0;多段场景逐段独立映射(`build_remaps` 返回各段映射;存在跳过段时对应段不入映射);无 `hidx` 不再豁免,段内槽位不一致同样 → `Corrupted`;载入期二次校验:重排映射指向不存在槽位 → `Corrupted` | `src/persist/recover/state/tests.rs::build_remaps_maps_slots_in_version_chain_order`、`src/persist/recover/state/tests.rs::build_remaps_rejects_out_of_range_duplicate_or_unreferenced_slots`、`src/persist/store/open/tests.rs::load_index_rejects_remap_past_state_slots` | Passed |
| FC-PERSIST-ERR-010 | ERR | msec 轻量索引区结构畸形(字段类别未知、区尾残留、倒排 offset/length 越界或相加溢出、词频合并溢出、bloom 位数/哈希数非法、doc 区 `doc_len = 0`(会使 BM25 `avgdl = 0` 产生 NaN 分数))与记录体字段标志畸形(如 `valid_time` 的 `has_to` 非 0/1)→ `Corrupted`,绝不 panic / 回绕 / 静默截断;fail-fast 打开时上报,非 fail-fast 时降级全量重建(索引是加速器而非数据源) | `src/persist/msec/index/tests.rs::malformed_regions_are_rejected`、`src/persist/msec/inverted/tests.rs::malformed_inverted_is_rejected_without_panic`、`src/persist/recover/state/tests.rs::malformed_region_section_is_error`、`src/persist/msec/inverted/tests.rs::decode_inverted_never_panics_on_arbitrary_bytes`、`src/persist/msec/index/tests.rs::decode_regions_never_panics_on_arbitrary_bytes`、`src/persist/msec/entry.rs::invalid_valid_time_flag_is_rejected` | Passed |
| FC-PERSIST-ERR-011 | ERR | msec `delta` 区结构畸形(kind 未知、长度越界或相加溢出、条目区 CRC 不符、区尾残留)→ `Corrupted`,绝不 panic / 回绕 / 静默截断;编码条目数超 `u16::MAX` → `LimitExceeded`,绝不钳制计数;非 fail-fast 打开时该段按损坏跳过或降级重建,绝不返回错误数据 | `src/persist/msec/delta/tests.rs::malformed_delta_is_rejected`、`src/persist/msec/delta/tests.rs::delta_count_overflow_is_rejected` | Passed |
| FC-PERSIST-ERR-012 | ERR | WAL/段/MANIFEST 恢复或提交中 `rowid = u64::MAX`、`ns_id = u32::MAX`、`next_segment_id = u32::MAX`、`manifest_version = u64::MAX` 使水位 `checked_add(1)` 失败 → `Corrupted`(恢复期)/`IdExhausted`(分配与提交期),绝不 `+1` 溢出 panic(debug)或回绕复用 ID(release);`RowId`/`NsId`/`SeqNo`/段号/MANIFEST 版本分配器空间耗尽 → `IdExhausted`,绝不回绕/panic | `src/persist/recover/wal_replay.rs::wal_replay_rejects_max_rowid`、`src/persist/recover/wal_replay.rs::wal_replay_rejects_max_ns_id`、`src/memory/table/state/tests.rs::id_allocators_reject_exhaustion`、`src/persist/manifest/tests.rs::manifest_counters_reject_exhaustion` | Passed |
| FC-PERSIST-ERR-013 | ERR | WAL 单帧负载(加密时按信封后字节)超 `Limits.wal_frame_max`(默认 16 MiB)→ `LimitExceeded { field: "wal 帧负载" }`,写事务回滚、该帧不落盘,绝不截断、绝不写出超限帧;限额经 `OpenOptions.limits` → `WalConfig.frame_max` 在写入路径 `WalWriter::append` 强制;回放侧 `visit_frames` 以「帧结束偏移 ≤ 文件长度」拒绝越界声称(撕裂尾部),不预分配 | `src/persist/store/wal_writer/tests.rs::frame_payload_over_limit_is_rejected`、`tests/persist_contracts.rs::wal_frame_limit_rejects_oversized_write` | Passed |
| FC-PERSIST-STA-001 | STA | 段生命周期:`Building → Committed → Obsolete → (trash)`;`Committed` 段内容不可变(write-once,重写产生新段) | `tests/persist_contracts.rs::committed_segment_is_write_once` | Passed |
| FC-PERSIST-STA-002 | STA | 崩溃点状态:`Building` 段(`.tmp` 半成品)与 MANIFEST 未引用的段为孤儿,可写打开时清理;不进入任何 MANIFEST 视图 | `tests/persist_contracts.rs::orphan_tmp_cleaned_on_open`、`tests/persist_contracts.rs::unreferenced_segment_cleaned_on_open` | Passed |
| FC-PERSIST-STA-003 | STA | 首次 flush 中途崩溃(段已写、MANIFEST 未提交):存在 WAL 时以 WAL 为准重建,孤儿段被清理,绝不误判为 `Corrupted` 而丢数据 | `tests/persist_contracts.rs::first_flush_crash_recovers_from_wal` | Passed |
| FC-PERSIST-STA-004 | STA | 多段 MANIFEST 提交:增量段 **append** 与 compaction 组 **replace** 均先写新 `MANIFEST.<v>` 再原子换 `current`;任意时刻活跃段集合 = 某 MANIFEST 版本所列集合;提交失败不改变活跃集合,已提交段 write-once;**大规模 flush 可切块构建并一次追加多段**(切块:每块至多 `Tuning.flush_chunk_rows` 行、默认 65_536,块数 = ⌈行数/块行数⌉,小规模保持单段;块级并行度 = `Tuning.flush_threads`、默认 **1(串行)**,切块行数与并行度一律经 `Builder::tuning` 显式注入(库本体不读环境变量,`FC-GLOBAL-INV-001`);块内 HNSW 构建用 `Builder::parallelism` 的批内并行,块级并行 >1 时内层降为 1 避免嵌套过度订阅),跨段 delta 只随首段写入,恢复语义不变 | `tests/l5_contracts.rs::incremental_flush_appends_segments`、`tests/l5_contracts.rs::compaction_bounds_segment_count`、`tests/l5_contracts.rs::compact_cleanup_failure_after_commit_still_succeeds`、`tests/persist_contracts.rs::large_flush_splits_into_parallel_segments`(规模用例,`#[ignore]`)、`src/persist/store/snapshot/tests.rs::split_slot_chunks_covers_all_slots_in_order`、`src/persist/store/snapshot/tests.rs::split_slot_chunks_limits_block_rows` | Passed |
| FC-PERSIST-POST-007 | POST | 段读取后端等价:feature `mmap` 开/关时 `source::read_whole` 与 `std::fs::read` 逐字节一致;`MmapSource::slice` 返回整段、`read_at` 越界 → `UnexpectedEof`(mmap 为优化,不改变功能语义;32 位平台长度转换失败返回 `Io`,不静默截断,解析证明登记) | `src/persist/source/tests.rs::read_whole_matches_bytes`、`src/persist/source/tests.rs::mmap_source_slice_and_bounds` | Passed |
| FC-PERSIST-POST-008 | POST | msec 轻量索引四区(字段字典 / zone map / bloom / 倒排)与内存结构往返一致:`flush` 全量写入;`open` 校验区结构并从磁盘倒排直接重建(经"段内槽位 → 全局槽位"重排映射),zone map 从槽位重建(等价);重开后 BM25 结果与分数逐位一致,段与未落盘增量共用同一全局统计(I21);四区为当前格式**必填**,任一缺失/畸形 → `Corrupted`(`fail_fast` 上报,否则降级为从槽位全量重建,两条路径等价);字段字典中 `key` 保留字段唯一(同名 metadata 不注册);`ttl_map` 区(块级 TTL 剪枝,FC-LIFE-CPLX-001) | `tests/l4_contracts.rs::text_index_survives_reopen`、`tests/l4_contracts.rs::flushed_and_tail_records_share_bm25_statistics`、`tests/l4_contracts.rs::lossy_zone_intervals_survive_fail_fast_reopen`、`tests/l4_contracts.rs::reopen_after_update_remaps_text_index`、`src/persist/flush/tests.rs::field_dict_keeps_single_key_field`、`src/persist/msec/index/tests.rs::ttl_map_roundtrip_and_invalid_tail` | Passed |
| FC-PERSIST-POST-009 | POST | 文本分词口径(停用词开关)建库即锁定:新库写入 MANIFEST(1=关、2=开,其余值 → `Corrupted`);`open` 忽略调用方冲突配置并以磁盘值为准(同时锁定查询分词),保证索引分词与查询分词同口径、绝不静默漏召回 | `tests/l4_contracts.rs::stopwords_setting_is_locked_at_creation`、`src/persist/manifest/tests.rs::stopwords_flag_roundtrip` | Passed |
| FC-PERSIST-POST-010 | POST | msec `delta` 区(设计 04 §2.2a)编解码往返一致:条目按 `(target, seqno, kind)` 排序,恢复时读入并回放访问统计与关系变更;`Access` delta 仅当 `seqno` 不早于该 RowId 最新版本行时才累加(版本行的 `access` 列**始终**为写入时刻累计快照,缺失会令更旧版本的值残留后与 delta 重复累加;同一 `RowId` 的后续版本必须覆盖,其**首个版本**为全零时可省略——零值与缺省 `AccessStat::default()` 等价,不产生残留);compaction 必须把被合并段中「其 RowId 最新版本未被新段覆盖」的 `Access` delta 携带进新段;区内条目数与 CRC 校验通过;空区表示本段无跨段变更,解码为空 `Vec` | `src/persist/msec/delta/tests.rs::delta_roundtrip_is_sorted_and_lossless`、`src/persist/msec/delta/tests.rs::empty_delta_is_valid`、`tests/l5_contracts.rs::delta_access_and_relations_survive_reopen`、`tests/l5_contracts.rs::access_delta_is_not_double_counted_after_later_version`、`tests/l5_contracts.rs::compaction_carries_access_delta_for_unmerged_rows`、`tests/l5_contracts.rs::compaction_keeps_access_dirty_for_history_only_row`、`tests/l5_contracts.rs::compaction_latest_in_keep_drops_covered_delta`、`tests/l5_contracts.rs::touch_then_update_single_flush_does_not_double_count` | Passed |
| FC-PERSIST-POST-011 | POST | WAL 轮转与 Checkpoint:单文件达 `CompactionPolicy.wal_file_bytes` 换新文件、单批不跨文件;打开按文件序回放,`seqno ≤ watermark` 的帧跳过(**注册表 metadata 帧同样带真实 seqno 并受水位约束**),残留旧文件不得复活已注销命名空间;Checkpoint 后已完全覆盖的旧 WAL 文件可删除(删除失败仅残留,恢复时按 `seqno ≤ watermark` 跳过),未覆盖前缀绝不丢;撕裂尾按 `FC-PERSIST-INV-005` 处理 | `tests/l5_contracts.rs::wal_rotation_splits_files_without_losing_batches`、`tests/l5_contracts.rs::checkpoint_removes_covered_wal_files`、`tests/l5_contracts.rs::stale_wal_files_do_not_resurrect_unregistered_namespace`、`tests/l5_contracts.rs::namespace_registered_after_last_flush_survives_crash`、`src/persist/store/wal_writer/tests.rs::wal_index_parsing_accepts_canonical_names_only` | Passed |
| FC-PERSIST-POST-012 | POST | **槽位归属恢复**:打开时按各段重排映射回填"槽位 → 所属段",段按编号升序回放(防御 MANIFEST 乱序);`unpersisted_slots` 只列 WAL 尾部新槽位,重开后空 flush 为空操作、compaction 不得把已落盘段当未落盘而整体丢弃 | `tests/l5_contracts.rs::reopen_preserves_segment_membership`、`src/persist/recover/state/tests.rs::collect_versions_orders_segments_by_id` | Passed |
| FC-PERSIST-POST-013 | POST | **冷启动门槛**:已 flush 的持久库(带 hidx)重开(`Builder::build`)耗时 < 1s(任意用例规模均断言);**正式规模门槛为 1M×1536,需 ≥16GB 专用 runner 手动执行**(heavy 用例默认 `#[ignore]`,`MNEME_HEAVY=1` 启用;回归建议 100_000×128,`MNEME_HEAVY_ROWS`/`MNEME_HEAVY_DIM` 可配,本地默认 50_000×128 冒烟);打开后首次点读与检索结果与关闭前/暴力对照一致,惰性驻留对读语义透明(FC-PERSIST-INV-021);空间上向量区不以堆拷贝驻留 | `tests/cold_start.rs::cold_open_under_one_second`(heavy;规模 env 见设计 14 §4,正式 1M×1536 见 §验收环境要求) | Planned |

---

## 3. 索引与检索(index/query/score)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-INDEX-INV-005 | INV | **I5**:同一快照内 `execute()` = 候选集内暴力 + 标准融合(统计等价;向量通道由 `tests/query_contracts.rs` 与 `tests/l4_contracts.rs` 双重验收,双通道融合见 `FC-QUERY-POST-004`) | `tests/l4_contracts.rs::vector_channel_matches_bruteforce`、`tests/query_contracts.rs::brute_force_matches_reference` | Passed |
| FC-INDEX-INV-006 | INV | **I6**:过滤先行;候选集内融合后截断,绝不"先融合截断再过滤";`top_k` 小于候选数时结果集仍以过滤后候选为准(构造低重要度高排名数据证伪后过滤实现) | `tests/l4_contracts.rs::filter_is_order_independent` | Passed |
| FC-INDEX-INV-021 | INV | **I21**:BM25 统计按查询命名空间跨全部活跃段全局聚合(df/N/avgdl),只计活行,与段数无关,跨 NS 互不影响(段与未落盘增量共用同一内存倒排) | `tests/l4_contracts.rs::bm25_statistics_are_namespace_isolated`、`tests/l4_contracts.rs::flushed_and_tail_records_share_bm25_statistics`、`src/query/bm25.rs::deleted_records_are_not_counted`、`src/memory/analysis/inv.rs::accumulates_tf_and_doc_len`、`src/memory/analysis/inv.rs::namespaces_are_isolated`、`src/memory/analysis/inv.rs::empty_text_is_not_indexed` | Passed |
| FC-INDEX-PRE-001 | PRE | 建库时校验 HNSW 参数:`m ≥ 2`、`m0 ≥ m`、`ef_construction ≥ 1`、`m`/`m0 ≤ 4096`(硬上限,越限使自产 hidx 无法读回)、`ef_search ≥ 1` 且 `ef_search ≤ Limits.ef_max`(防止默认查询宽度绕过查询期上限);过滤三档阈值 `filter_post_threshold`/`filter_brute_threshold` 为 `[0,1]` 内有限值且 `brute ≤ post`;`bloom_fpp ∈ (0,1)` 且有限、`field_dict_max ≥ 1`(极小 `fpp` 会产出 `k > 64` 的不可读回段;字段上限 0 使 key bloom 缺失);建图/建段工程调参(选邻比较上限 `hnsw_compare_cap`、批行数 `hnsw_batch_rows`、小图串行阈值 `hnsw_serial_rows`、建图线程上限 `hnsw_threads_max`、切块行数 `flush_chunk_rows`、块级并行 `flush_threads`)均须 ≥ 1。违反 → `Config`/`LimitExceeded`,绝不静默 | `tests/hnsw_contracts.rs::invalid_hnsw_params_and_thresholds_are_rejected`、`tests/hnsw_contracts.rs::build_tuning_knobs_are_validated`、`tests/l4_contracts.rs::builder_rejects_invalid_bloom_fpp` | Passed |
| FC-INDEX-POST-001 | POST | 过滤三档:①后过滤(`s > post`,全图遍历 + `ef' = max(ef,k)·min(8,1/s)`);②全图遍历 + `ef' = max(ef,k)·4` 后过滤(结果限候选,触发条件 `brute < s ≤ post` 且候选数 ≥ `max(ef,1024)`;不用约束遍历以免图被过滤切断);③候选暴力(**仅当存在过滤位图**:选择性 ≤ `brute_threshold` **或**候选数 < `max(ef,1024)`,两个触发条件各自独立成立;无过滤时不入档③(无过滤时候选即 alive、选择性恒为 1;默认 `post < 1` 走档①,合法边界 `post = 1.0` 走档②,二者同为全图遍历、不改变语义));档③恒等于「候选位图内暴力」,档①②与之统计等价(口径:`ef'·s ≳ 4k`,`ef→∞` 精确;设计 05 §8) | `src/index/filtered.rs::tier_selection_matches_selectivity_and_candidate_cap`、`tests/hnsw_contracts.rs::filter_tier_three_matches_candidate_bruteforce`、`tests/hnsw_contracts.rs::filter_brute_trigger_conditions_are_independent`、`tests/hnsw_contracts.rs::filter_post_tier_matches_candidate_bruteforce`、`tests/hnsw_contracts.rs::filter_amplified_tier_matches_candidate_bruteforce` | Passed |
| FC-INDEX-POST-002 | POST | **I5 收敛**:`ef → ∞` 时 HNSW 结果收敛于精确暴力 | `tests/hnsw_contracts.rs::ann_converges_to_bruteforce_with_large_ef` | Passed |
| FC-INDEX-POST-003 | POST | **排序全等性**:同一快照内任意两次 `execute()`(同参数)结果完全一致(同分按 RowId 升序) | `tests/query_contracts.rs::search_order_is_total_and_stable` | Passed |
| FC-INDEX-POST-005 | POST | ANN 结果 ⊆ alive ∩ 过滤位图,**且 alive 按目标命名空间与 TTL 判定**(其他 NS 与逻辑过期记录不得入选);死节点与未被 alive 选中的历史版本只可穿越、不可入选(设计 05 §7/§12);`as_of` 历史视图按历史 alive 位图返回已删记录、当前视图不返回,且 `snapshot_at` 保留索引句柄(不得静默降级暴力) | `tests/hnsw_contracts.rs::ann_excludes_deleted_records`、`tests/hnsw_contracts.rs::ann_respects_namespace_and_ttl_visibility`、`tests/hnsw_contracts.rs::ann_after_as_of_matches_bruteforce`、`src/memory/temporal.rs::snapshot_at_preserves_index_handle`、`src/index/hnsw/tests.rs::search_results_respect_alive_bitmap` | Passed |
| FC-INDEX-POST-007 | POST | hidx(HID1)编解码往返恢复同一图(节点数/层级/邻接/入口/参数);`decode(encode(g))` 与 `g` 一致 | `src/index/hidx/tests.rs::hidx_roundtrip_restores_graph` | Passed |
| FC-INDEX-POST-008 | POST | 持久库 `flush` 写 `hidx` 并在 MANIFEST 登记 `hidx_crc`/`entry_slot`/`entry_level`;重开经重排映射从 hidx 载入索引(`stats().segments[*].index_nodes > 0`)且检索正确;重排映射在非恒等场景(删除/多版本导致段内槽位次序与全局槽位次序不同)亦正确(小 `ef` 多查询召回 + `ef→∞` 精确双重验收) | `tests/hnsw_contracts.rs::reopen_loads_hnsw_from_hidx`、`tests/hnsw_contracts.rs::reopen_after_delete_remaps_slots` | Passed |
| FC-INDEX-POST-009 | POST | ANN Recall@10 ≥ 0.95(`ef=128`,`HnswParams::default()`,段行数超过 `brute_force_max_rows` 时走图);**分派为严格「超过」:行数 ≤ `brute_force_max_rows` 时恒暴力,与索引是否存在无关**;两种固定种子分布(随机均匀与 8 簇合成数据)分别达标 | `tests/hnsw_contracts.rs::ann_recall_at_ten_meets_threshold`、`src/memory/search/tests.rs::search_dispatches_to_index_when_prefix_exceeds_brute_threshold`、`src/memory/search/tests.rs::search_bruteforces_when_prefix_does_not_exceed_threshold` | Passed |
| FC-INDEX-INV-007 | INV | 图节点 id ∈ [0,count);每层度数 ≤ M0(第 0 层)/ M(上层);无自环、邻居 id 有效;入口节点层级 = 全图最高层。构建与 hidx 载入两条路径恒成立(载入解码即校验,违反 → `Corrupted`) | `src/index/hnsw/tests.rs::graph_degree_and_self_loop_invariants`、`src/index/hidx/tests.rs::hidx_rejects_degree_above_layer_bound`、`src/index/hidx/tests.rs::hidx_rejects_entry_level_below_max` | Passed |
| FC-INDEX-INV-008 | INV | 查询 = 各段索引 ANN + 未落盘尾部暴力,`TopK` 归并;`ef→∞` 时结果 ≡ 全量候选暴力(设计 05 §9;多段形态下每段独立分派过滤三档)。**纯内存库同口径**:`Mneme::flush` 把未覆盖槽位建成**内存段**(同一 `IndexFactory` 建图,但不写 vsec/msec/hidx、不做量化副本,`quant = F32`),此后查询走「内存段 ANN + 尾部暴力」,与持久库逐字节同图、同结果;无新增槽位时为空操作(不产段) | `tests/hnsw_contracts.rs::ann_merges_prefix_with_unflushed_tail`、`tests/l5_contracts.rs::multi_segment_search_matches_bruteforce`、`tests/hnsw_contracts.rs::in_memory_flush_builds_ann_segments` | Passed |
| FC-INDEX-ERR-001 | ERR | hidx 魔数不符/负载 CRC 翻转 → `Corrupted`;格式版本不一致 → `UnsupportedVersion`(I18);头部 `ef_construction = 0`/入口层级低于最高层/逐层度数越界/截断/头 CRC/布局不符 → `Corrupted`;**载入节点数与恢复槽位数不一致 → `Corrupted`**;**量化副本格式/行数/单行码长/i8 参数表长度/f16 维度与索引节点或维度不一致 → `Corrupted`,绝不按错误码流打分**;任意输入不 panic、不静默(proptest「接受即往返」+ 定向用例) | `src/index/hidx/tests.rs::hidx_rejects_bad_magic`、`src/index/hidx/tests.rs::hidx_detects_payload_corruption`、`src/index/hidx/tests.rs::hidx_rejects_version_mismatch`、`src/index/hidx/tests.rs::hidx_rejects_zero_ef_construction`、`src/index/hidx/tests.rs::hidx_rejects_entry_level_below_max`、`src/index/hidx/tests.rs::hidx_rejects_degree_above_layer_bound`、`src/index/hidx/tests.rs::hidx_rejects_truncated_or_malformed_header`、`src/index/hidx/tests.rs::hidx_rejects_bad_layout`、`src/index/hidx/tests.rs::hidx_rejects_bad_neighbors`、`src/index/hidx/tests.rs::hidx_decode_never_panics_on_arbitrary_bytes`、`src/index/hnsw/tests.rs::load_rejects_node_count_mismatch`、`src/index/hnsw/tests.rs::validate_quant_rejects_malformed_copies` | Passed |
| FC-INDEX-ERR-002 | ERR | MANIFEST 引用的 `hidx` 缺失/整文件 CRC 不符:fail-fast 打开 → `Corrupted`;可写非 fail-fast 打开 → 降级暴力(`stats().segments[*].index_nodes == 0`)、库仍可读,`db.check()` 报告该段损坏 | `tests/persist_contracts.rs::missing_hidx_degrades_or_rejects`、`tests/persist_contracts.rs::corrupt_hidx_degrades_or_rejects` | Passed |
| FC-INDEX-ERR-003 | ERR | `hidx::encode` 编码期防御:图节点数/节点表/邻接区字节数超 `u32` → `LimitExceeded`(`field` 标明具体字段);单层度数超 `u16` → `Inconsistent`(违反 `FC-INDEX-INV-007` 度数上界)。绝不静默截断(`as u32`/`as u16`)。`Builder` 校验(度数 ≤ 4096)下正常构建不可达;长度分支需 >4 GiB 邻接区,64 位平台以解析证明登记,度数分支以定向测试证伪 | `src/index/hidx/tests.rs::hidx_encode_rejects_degree_above_u16` | Passed |
| FC-SCORE-INV-027 | INV | **I27**:同一 `(rowid, query_id)` 的反馈至多计一次;对不可见记录(不存在/已墓碑/已过期)的反馈返回 `false` 且**不占用幂等键**(后续该 `RowId` 重新可见时首次反馈仍生效);`execute()` 缺省生成的 `QueryId` 由进程级全局分配器分配(跨库实例共享同一编号空间,保证不冲突),调用方显式指定时须自行保证唯一性 | `tests/query_contracts.rs::feedback_is_idempotent_per_query` | Passed |
| FC-SCORE-POST-001 | POST | `Scoring::default()` 与未开启 `score()` 的排序全等 | `tests/query_contracts.rs::default_scoring_matches_similarity_order` | Passed |
| FC-SCORE-POST-002 | POST | `Scoring::floor` 下的候选满足 `ŝ ≥ floor` 或 `S = 0`:归一化相似度 `ŝ < floor` 时综合分清零,`ŝ = floor` 为保留边界(实现用严格小于,等于保留);`floor = 0` 时恒不清零 | `tests/query_contracts.rs::scoring_floor_zeroes_below_threshold` | Passed |
| FC-SCORE-POST-003 | POST | 综合排序的**候选放大**:`Scoring` 开启任一非相似度因子(任一权重 ≠ 0 或 `floor > 0`)时,向量通道 ANN 探查宽度放大为 `ef' = max(ef, 4·top_k)`;相对暴力综合排序的 Recall@k ≥ 0.98(相对召回损失 ≤ 2%);默认 `Scoring`(仅相似度)与未开启时不放大,排序与 `FC-SCORE-POST-001` 全等 | `tests/l4_contracts.rs::scoring_amplification_preserves_recall` | Passed |
| FC-SCORE-POST-006 | POST | **联想扩展 max 合并**:扩展候选与既有通道候选按 `RowId` 合并,分数取 `max(自身分, boost)`(同一 `RowId` 绝不重复出现);`Hit.via` 仅在扩展确有贡献(新增候选或提升分数)时记录来源边;`hops`/`decay`/`max_nodes` 语义不变(`FC-SCORE-CPLX-002`、`FC-SCORE-POST-004`) | `tests/query_contracts.rs::expansion_max_merges_with_existing_candidates` | Passed |
| FC-SCORE-POST-007 | POST | **重要性偏置路由**(`Scoring::bias_routing = true`):HNSW 遍历的前沿出堆顺序按 `priority = close_key(score) + β·(importance + min(access/c_norm, 1))`(β=1.0 内部固定)偏置,**仅改访问顺序**:返回候选的分数与 `ef→∞` 结果同关闭时一致,最终排序由综合重排决定;不改变候选集上界与 `FC-INDEX-CPLX-002` 复杂度 | `tests/query_contracts.rs::bias_routing_only_changes_visit_order` | Passed |
| FC-SCORE-POST-004 | POST | 联想扩展只沿**同命名空间**的边推进:目标槽位 `ns_id` 与发起检索的命名空间不一致时视为不存在,绝不把其他命名空间的记忆引入结果(`relate` 可记录跨命名空间边,但扩展不越界) | `tests/query_contracts.rs::expansion_does_not_cross_namespaces` | Passed |
| FC-SCORE-POST-005 | POST | 结果级去重(`ResultDedup`):`Off` 原样返回;`ById` 按 `RowId` 保留首次出现(输入按分数有序,即保留最高分);`Near { threshold }` 按输入顺序贪心保留,与任一已保留项余弦相似度 ≥ `threshold` 者丢弃;`threshold` 须为 `[0,1]` 内的有限值,越界或非有限值在 `execute()` 入口 → `Config`,绝不静默空转 | `tests/query_contracts.rs::result_dedup_modes_and_threshold_validation` | Passed |
| FC-QUERY-ERR-001 | ERR | DSL 任意输入不 panic,返回结构化 `FilterParse`(I7);解析器设嵌套深度上限,错误携带字节位置;数字非有限(如 `1e999`)与相对时间量超出 `f64` 精确范围/ISO 8601 可往返范围(4 位年份) → `FilterParse`,绝不整数溢出 panic,也绝不产出 `Display` 读不回的值 | `tests/l4_contracts.rs::dsl_never_panics_on_arbitrary_input`、`src/query/parse/tests.rs::malformed_inputs_report_position`、`src/query/parse/tests.rs::out_of_range_numbers_and_durations_are_rejected`、`src/query/parse/tests.rs::deeply_nested_input_is_rejected_without_panic` | Passed |
| FC-QUERY-ERR-002 | ERR | `Not` 对缺失字段采用三值语义(缺失 → `Not` 亦为 false) | `tests/query_contracts.rs::filter_uses_kleene_three_valued_logic` | Passed |
| FC-QUERY-POST-001 | POST | 谓词类型规则:数值比较 `Int`/`Num` 互通;`Ts` 仅与 `Ts` 比较;`Contains`/`StartsWith`/`EndsWith`/`Glob` 要求字符串或数组;类型不匹配求值为 `Unknown`(不命中) | `tests/query_contracts.rs::predicate_type_rules` | Passed |
| FC-QUERY-POST-002 | POST | **DSL 往返**:解析 → `Display` → 再解析等价;解析 → `to_meta` → `from_meta` 等价;`always`/`never` 常量闭合,程序构造的空 `And`/`Or`/`In` 规约为等价真值常量(设计 06 §1) | `tests/l4_contracts.rs::dsl_display_and_json_roundtrip`、`src/query/display.rs::empty_lists_print_as_truth_constants`、`src/query/json.rs::json_encodes_empty_lists_as_truth_constants` | Passed |
| FC-QUERY-POST-003 | POST | **BM25 打分**(设计 06 §3.2):`k1=1.2`、`b=0.75`;IDF 稀有词得分更高、TF 饱和(有界 `k1+1`)、长度归一(同 tf 短文档更优);精确分数与手算公式一致 | `tests/l4_contracts.rs::bm25_formula_behaviour`、`tests/l4_contracts.rs::bm25_exact_scores_match_formula`、`src/query/bm25.rs::rare_term_ranks_above_common_term`、`src/query/bm25.rs::term_frequency_saturates`、`src/query/bm25.rs::shorter_document_wins_at_equal_tf` | Passed |
| FC-QUERY-POST-004 | POST | **双通道融合**(设计 06 §4):默认 `Rrf{k:60}`(只比名次);`Weighted` 在本次结果集内 min-max 归一(Euclidean 距离先取负;零极差通道(单点或全同分)归一值取 1,极小非零极差仍按公式缩放,极端极差不产生 NaN);同分按 `RowId` 升序;`Fusion` 未同时启用双通道或 `Weighted.alpha` ∈ `[0,1]` 外/非有限 → `Config` | `tests/l4_contracts.rs::hybrid_fusion_and_validation`、`src/query/fusion.rs::rrf_matches_design_example`、`src/query/fusion.rs::weighted_flips_distance_channel`、`src/query/fusion.rs::weighted_single_result_channel_normalizes_to_one`、`src/query/fusion.rs::weighted_tiny_span_still_scales`、`src/query/fusion.rs::weighted_extreme_span_stays_finite`、`src/query/fusion.rs::ties_break_by_rowid` | Passed |
| FC-QUERY-POST-005 | POST | **计划器等价性**(设计 06 §2):zone map/bloom 块位图只剪"必然不命中"的块;`Not` 与无摘要条件保持全 1;类型不匹配、`f64` 无法精确表示的整数(绝对值 > 2^53)一律返回全 1;**与保留字段同名的 metadata 一律不进 zone map**(`rowid`/`key`/`__ns`/`access_count`/`last_access` 及 `created_at` 等系统字段的行级值优先于同名 metadata,据 metadata 统计剪枝会漏报),但保留名对象下的**子路径**(如 `key.x`)按 `meta::get_path` 语义照常观察;非数值字段不参与区间统计(只保证不漏报),`exists` 以"字段是否出现(含非数值/null)"判定;最终候选与逐行三值求值全等 | `tests/l4_contracts.rs::plan_filter_matches_pointwise_count`、`tests/l4_contracts.rs::planner_never_prunes_possible_blocks`、`src/query/plan/tests.rs::plan_candidates_match_bruteforce`、`src/query/plan/tests.rs::block_pruning_skips_impossible_blocks`、`src/memory/analysis/zones/tests.rs::kind_conflict_disables_block_pruning`、`src/memory/analysis/zones/tests.rs::reserved_metadata_is_shadowed_and_not_indexed`、`src/memory/analysis/zones/tests.rs::reserved_object_subpaths_are_still_indexed`、`src/query/plan/tests.rs::reserved_metadata_shadowing_never_prunes`、`src/query/plan/tests.rs::dotted_reserved_subpath_never_prunes`、`src/memory/pred_eval/tests.rs::reserved_names_resolve_to_reserved_values`、`src/query/plan/tests.rs::key_bloom_rejects_absent_key`、`src/query/plan/tests.rs::key_bloom_skips_row_evaluation` | Passed |
| FC-QUERY-POST-006 | POST | **历史视图 TTL**(设计 07):`as_of(t)` 检索的 TTL 可见性以视图时刻 `t` 为准(`expires_at > t`),与墙上时钟无关;`SnapshotHandle` 路径同样以快照时刻判定 | `tests/l4_contracts.rs::historical_search_uses_view_time_for_ttl` | Passed |
| FC-QUERY-POST-008 | POST | **计划/段位图的视图级缓存语义透明**(设计 06 §2):无用户过滤且目标命名空间(段)内无任何 `expires_at` 行时,同一不可变视图内的重复查询复用「可见候选/段 alive 位图」缓存;缓存命中与逐行三值求值的结果**逐位全等**;命名空间(段)内存在 TTL 行时**不写缓存**(过期随时间实时反映,过期行绝不因缓存复活);写事务发布新视图后旧快照缓存随之失效(缓存仅挂不可变 `ReaderView`,绝不跨视图复用);缓存不改变复杂度声明(首次 $O(N)$、同视图重复查询为 $O(1)$ 取用) | `src/query/plan/tests.rs::plan_cache_is_transparent_and_ttl_aware`、`src/memory/search/tests.rs::segment_alive_cache_is_transparent_and_view_scoped` | Passed |
| FC-QUERY-POST-009 | POST | **残余谓词选择性重排**(设计 06 §2):计划编译时对 `And` 合取链按**预估选择性升序**稳定重排(等值/`In`/`IsNull`/`Never` 等高选择分支在前,`Exists`/`Ne` 等低选择分支在后;嵌套 `And` 递归,`Or`/`Not` 结构保持),重排**仅改变求值顺序**:候选集合、块级剪枝与逐行原 AST 三值求值全等(三值 `And` 可交换可结合);高选择分支的短路必须真实减少昂贵子谓词(`Contains`/`StartsWith`/`EndsWith`/`Glob`/`In`)的求值次数 | `src/memory/pred/tests.rs::reorder_sorts_conjuncts_by_estimated_selectivity`、`src/query/plan/tests.rs::reordered_conjunctions_match_bruteforce_and_short_circuit` | Passed |
| FC-QUERY-POST-007 | POST | ISO 8601 ↔ Unix 毫秒(`query::iso`):4 位年份、时间部分(含 `T`)整体可缺省,`T` 后 `hh:mm` 必填且时/分/秒各**恰好 2 位**、秒与小数秒可缺省;`T`/`Z` 大小写兼容,时区 `±hh:mm`/`±hhmm`(时分各恰好 2 位);小数秒最多 9 位、截断到毫秒且不足 3 位按十分位/百分位补零(绝不读入后续时区字符);秒域 0–59(不支持闰秒 60);日历越界(2 月 30 日等)拒绝而非归一;`[MIN_ROUNDTRIP_MS, MAX_ROUNDTRIP_MS]` 内 `parse(format(ms)) == ms` | `src/query/iso.rs::parses_with_millis_and_offset`、`src/query/iso.rs::date_only_defaults_to_midnight_utc`、`src/query/iso.rs::accepts_lowercase_and_compact_offset`、`src/query/iso.rs::rejects_calendar_and_format_violations`、`src/query/iso.rs::pre_epoch_and_roundtrip`、`src/query/iso.rs::format_parses_back_for_sample_range`、`src/query/iso.rs::roundtrip_covers_range_samples`、`src/query/iso.rs::roundtrip_bounds_cover_exact_range`、`src/query/iso.rs::fractional_seconds_pad_to_millis` | Passed |
| FC-INDEX-POST-004 | POST | 写入期去重:`Dedup::Merge` 就地更新并保留旧 RowId(返回 `Merged(old)`);`Dedup::Replace` 生成新 RowId 并墓碑旧行;`insert_batch` 中 `RejectDuplicate`/`Dedup::Reject` 逐条返回 `Duplicate`,不回滚整批 | `tests/memory_contracts.rs::dedup_reject_replace_and_merge`、`tests/memory_contracts.rs::dedup_replace_with_key_gets_new_rowid` | Passed |
| FC-INDEX-POST-010 | POST | **建图精度档位**(`BuildPrecision`,默认 `Hybrid`;设计 05 §4.4):`Hybrid` 档建图遍历(`search_layer`/`greedy_query`)用段级逐维 i8 码流近似距离(复用 `FC-QUANT-POST-001` 的段内统计与编码;**临时副本不落盘**,构建结束即释放),邻居选择/修剪前用 f32 原向量对候选重排;`F32` 档全 f32 精确(原行为)。两档共同成立:①图不变量 `FC-INDEX-INV-007`;②同配置同输入构建确定性(逐字节相同 hidx);③hidx 文件格式与档位解耦(头/节点表/邻接不含档位元数据,载入路径无差异);④档位语义可证伪:`build_codes` 在 `F32` 档返回 `None`、`Hybrid` 档返回段级码流且逐维解码误差 ≤ `Δ/2`(与 `FC-QUANT-POST-001` 同源);连续数据上 `Hybrid` 图与 `F32` 图结构不同(i8 遍历路径端到端生效)。注:两档距离数值路径不同(码流×权重乘加 vs f32 点积),浮点求和次序不同,不承诺逐位同值 | `src/index/hnsw/tests.rs::hybrid_build_is_deterministic_and_valid`、`src/index/hnsw/tests.rs::build_codes_follow_precision`、`src/index/hnsw/tests.rs::hybrid_uses_approximate_distances_on_continuous_data` | Passed |
| FC-INDEX-POST-011 | POST | **低精度建图召回保真**:同一确定性数据集(随机均匀 + 8 簇两种分布,`ef=128`),`Hybrid` 与 `F32` 建图的 Recall@10 差 ≤ **0.02**;`Hybrid` 档自身召回 ≥ 0.95(默认档口径,与 `FC-INDEX-POST-009` 同门槛),且 `ef→∞` 集合相等不因档位破坏 | `tests/hnsw_contracts.rs::hybrid_and_exact_builds_have_close_recall`、`tests/hnsw_contracts.rs::hybrid_converges_to_bruteforce_with_large_ef` | Passed |
| FC-INDEX-POST-012 | POST | **批内并行建图**(设计 05 §4.4):构建按批行数分批——批内节点基于**批开始图快照**并行计算选邻计划(只读);应用分两步:先串行加边(无距离计算)并收集被触达的 `(节点,层)`,再把超员节点的**修剪邻接计算按批并行求值**(只读批末图快照、各改各的邻接表)、按 `(节点,层)` 序串行写回;并行执行器为**每次构建一个常驻 worker 池**(`thread::scope` 一次生成,任务/结果经 channel 分派;计划与修剪阶段 worker 持共享读锁、主线程只在两阶段之间持写锁应用,绝不并发读写),worker 数与批行数解耦——同一批的计划/修剪任务由固定 worker 领取、结果按位置回填;批行数 = 节点数 ≤ `Tuning.hnsw_serial_rows`(默认 64)时退化串行,否则 `Tuning.hnsw_batch_rows`(默认 128,4 核基准标定:128 维/1536 维构建较 8 快 10%–20% 且召回不变)——**只依赖节点数与配置**,与线程数/核数无关;首批行数受 `m0` 约束(冷启动核心入边不被集中修剪);线程数 = `min(Builder::parallelism(0=可用核数), Tuning.hnsw_threads_max(默认 8), 批行数)`;启发式选邻的候选-已选两两比较上限 = `Tuning.hnsw_compare_cap`(默认 4,只与最近选中的至多该数量比较);构建结束后对不可达节点做链式可达性修复。因此同输入同配置**逐字节同图**(跨线程数确定性),图不变量 `FC-INDEX-INV-007` 成立;建图/修剪线程 panic 收敛为 `Inconsistent` 结构化错误,绝不把 panic 抛给调用方 | `src/index/hnsw/tests.rs::build_batch_rows_depends_only_on_count`、`src/index/hnsw/tests.rs::parallel_build_is_thread_count_independent`、`src/index/hnsw/tests.rs::parallel_build_keeps_graph_reachable_from_entry`、`src/index/hnsw/tests.rs::graph_degree_and_self_loop_invariants` | Passed |

---

## 4. 记忆模型(model)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-MODEL-INV-022 | INV | **I22**:RowId 跨 `update`/upsert 不变,访问统计与关系边始终有效 | `tests/memory_contracts.rs::upsert_keeps_rowid_and_rejects_duplicate` | Passed |
| FC-MODEL-INV-024 | INV | **I24**:`update` 新版本对读者原子可见,旧版本立即遮蔽 | `tests/memory_contracts.rs::update_is_atomically_visible`、`tests/model_contracts.rs::concurrent_readers_never_see_torn_updates`、`tests/model_contracts.rs::model_version_lifecycle_states` | Passed |
| FC-MODEL-INV-025 | INV | **I25**:悬挂边不可见;删除一端后边立即失效,compaction 后物理清除 | `tests/model_contracts.rs::relate_is_idempotent_and_dangling_edges_hidden` | Passed |
| FC-MODEL-INV-026 | INV | **I26**:`as_of(t)` = 事务时间 ≤ t 的最新可见版本组成的一致快照,不随后续写入/compaction 变化;历史版本默认永久保留(`history_horizon=None`) | `tests/model_contracts.rs::as_of_returns_historical_snapshot`、`tests/model_contracts.rs::as_of_matches_reference_prop`、`tests/model_contracts.rs::model_version_lifecycle_states` | Passed |
| FC-MODEL-POST-001 | POST | `relate` / `relate_with_options` 以 `(from,to,kind)` 幂等 upsert(后者经 `RelateOptions` 同时覆盖 weight 与 metadata) | `tests/model_contracts.rs::relate_is_idempotent_and_dangling_edges_hidden`、`tests/model_contracts.rs::relate_is_idempotent_prop` | Passed |
| FC-MODEL-POST-002 | POST | `consolidate` 幂等:已沉淀簇跳过;`keep_sources=true` 不删除来源 | `tests/model_contracts.rs::consolidate_merges_cluster_and_keeps_sources` | Passed |
| FC-MODEL-POST-003 | POST | `supersede` 后旧版本 `valid_to` = 新版本 `valid_from`(历史可见性受 compaction 回收约束,I26);**信念修订沿用目标 key**:新记录省略 key 时继承首参 key,显式给出且冲突 → `KeyMismatch`,使 `key_index` 无悬挂、`get`/`check` 一致;**已墓碑记录(delete)与 `update` 同口径返回 `NotFound`,墓碑绝不因 `supersede` 复活** | `tests/model_contracts.rs::supersede_closes_previous_valid_to`、`tests/model_contracts.rs::supersede_preserves_key_and_rejects_conflict`、`tests/model_contracts.rs::supersede_after_delete_returns_not_found` | Passed |
| FC-MODEL-POST-004 | POST | **版本链保留**:每个 RowId 的最新版本(或最新墓碑,以维持当前可见状态)与 `tx_ms ≥ now − history_horizon` 的历史版本被 compaction 保留;仅 `tx_ms < now − horizon` 的历史版本可回收;**整链回收以最新版本为准**:最新版本为窗口外的墓碑/逻辑过期时整链(无论各历史版本 `tx_ms`)一并回收,绝不只回收 `latest` 而留下悬挂/旧活版本复活(时钟回拨下同样成立);整链逻辑过期同理;`as_of` 在窗口内不随后续写入/compaction 变化,`horizon = None`(默认)时永久保留且死比率不触发重写 | `tests/l5_contracts.rs::history_horizon_reclaims_old_versions`、`tests/l5_contracts.rs::compaction_reclaims_tombstones_under_horizon`、`tests/l5_contracts.rs::compaction_preserves_snapshot_and_as_of_within_horizon`、`tests/l5_contracts.rs::compaction_never_revives_deleted_record_under_clock_skew`、`src/life/compact/tests.rs::reclaims_whole_chain_when_latest_tombstone_outside_window`、`src/life/compact/tests.rs::keeps_latest_tombstone_inside_window_and_reclaims_history` | Passed |
| FC-MODEL-POST-005 | POST | `predecessors(to, kinds)` 只返回 `edge.to == to` 且 `edge.kind ∈ kinds`、两端存活的边;`RelationIndex::Outgoing` 与 `Both` 结果一致(反向索引只加速、不改语义) | `tests/model_contracts.rs::predecessors_returns_incoming_edges`、`tests/l5_contracts.rs::relation_index_modes_agree_after_reopen` | Passed |
| FC-MODEL-POST-006 | POST | `consolidate(policy)` 以 `threshold` 为聚类相似度下界:相似度 ≥ threshold 的近似重复聚为一簇,簇成员 ≥ 2 才合并;`keep_sources=true` 不删来源;策略参数非法(`threshold` 非有限值或越界 [0,1]、`max_cluster = 0`)→ `Config`,绝不静默空转或索引越界 | `tests/model_contracts.rs::consolidate_merges_cluster_and_keeps_sources`、`tests/model_contracts.rs::consolidate_rejects_invalid_policy` | Passed |
| FC-MODEL-POST-007 | POST | `RelationIndex::Both` 时段内 `relations` 区追加按 `(to, kind, from)` 排序的反向表;`edges::parse` 返回正向/反向两表;关系区带**全量/增量标志**(次版本 4 起):带 `FLAG_FULL` 的段(首段/compaction)恢复时先重置关系表再应用,增量段仅 upsert(其 relations 区为空表,关系变更由 delta 区承载,绝不因重置清掉先前段的边);compaction 新段为全量快照,被删除的旧边绝不因并集复活;恢复后入边集合与 `Outgoing` 模式逐边一致、重开不丢边;默认 `Outgoing` 不写反向表 | `src/persist/edges.rs::edges_roundtrip_with_reverse`、`tests/l5_contracts.rs::reverse_relation_table_survives_reopen`、`tests/l5_contracts.rs::compaction_does_not_resurrect_removed_edges`、`src/persist/recover/state/tests.rs::incremental_relations_are_upserted` | Passed |
| FC-MODEL-POST-008 | POST | **自定义关系类型注册表**(`Namespace::relation_kind(name)`):名称→稳定编号,内置名解析为内置 `0..=3`,新名称从 **16** 起单调分配且**幂等**(同名重复调用返回同编号);空名/含控制字符/超长(>128 B)→ `Config`;编号空间耗尽(`next_rel_kind == u16::MAX`)→ `TooLarge`,绝不回绕复用;注册经 WAL `RelKindRegister` 帧持久化、随 MANIFEST `rel_kinds`/`next_rel_kind` 落盘,重启与崩溃恢复(仅 WAL)后名称↔编号一致;同名不同编号或同编号不同名的冲突记录 → `Corrupted` | `tests/model_contracts.rs::custom_relation_kinds_are_stable_and_validated`、`tests/model_contracts.rs::manifest_rejects_conflicting_relation_kinds`、`tests/l5_contracts.rs::custom_relation_kind_survives_reopen`、`tests/l5_contracts.rs::custom_relation_kind_survives_crash`、`src/memory/table/state/tests.rs::relation_kind_registry_rejects_exhaustion` | Passed |
| FC-MODEL-STA-001 | STA | 记忆版本五元组 `M=(S={Active,Shadowed,Reclaimed}, E={Update,Upsert,Delete,AsOf,Compact}, δ: Active×{Update,Upsert,Delete}→Shadowed(旧)∧Active(新), Shadowed×AsOf→Shadowed(历史可见), Shadowed×Compact→Reclaimed, s0=Active, F={Reclaimed})`;非法转移:当前读路径(`get`/`search`/`iter`/`count`)命中 `Shadowed` 必须不可见并显式拦截,绝不静默返回 | `tests/model_contracts.rs::model_version_lifecycle_states` | Passed |

---

## 5. 生命周期(life)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-LIFE-PRE-001 | PRE | 建库校验 `CompactionPolicy`:`tier_ratio`/`tier_count` ≥ 2、`segment_rows` ≥ 1、`dead_ratio`/`io_budget` 为 `[0,1]` 内有限值;违反 → `Config`(避免触发条件永假或除零) | `tests/l5_contracts.rs::builder_rejects_invalid_compaction_policy` | Passed |
| FC-LIFE-INV-008 | INV | **I8**:活跃段数 ≤ `(T−1)·log_r(N/B)+c`(size-tiered:段按 `row_count` 分层,同层段数 < `tier_count` 否则触发合并;死比率超线时重写该段,**死比率只计窗口外可回收死行**,`horizon = None` 时无回收收益、不触发重写);WAL 总量 ≤ `12 × wal_bytes`(软/硬阈值口径见 `FC-PERSIST-INV-004`) | `tests/l5_contracts.rs::compaction_bounds_segment_count`、`tests/l5_contracts.rs::auto_compaction_triggers_in_background`、`tests/l5_contracts.rs::compact_without_horizon_does_not_rewrite_dead_segments`、`src/life/compact/tests.rs::dead_ratio_counts_only_reclaimable_versions` | Passed |
| FC-LIFE-INV-009 | INV | **I9**:逻辑过期/墓碑记录在常规读路径永不返回(仅 `iter_with(..., true)` 审计入口可见);内部辅助路径(dedup 判重、`stats` 计数、`consolidate` 候选、`forget` 目标)同样排除逻辑过期记录;物理回收仅在 compaction 提交后 | `tests/memory_contracts.rs::delete_hides_records_from_reads`、`tests/memory_contracts.rs::logically_expired_hidden_from_internal_paths` | Passed |
| FC-LIFE-INV-010 | INV | **I10**:compaction 任意步骤崩溃 → 恢复后数据集 = 提交前状态;新段为孤儿(下次启动清理),旧 MANIFEST 完好、无损回滚;提交以「写 `MANIFEST.<v>` → 原子换 `current`」完成 | `tests/l5_contracts.rs::compaction_failure_keeps_state_and_returns_idle` | Passed |
| FC-LIFE-INV-011 | INV | **I11**:备份目录独立 `open` + `check` 通过(**FC-PERSIST-POST-004** 已覆盖复制语义) | `tests/persist_contracts.rs::backup_is_independently_openable` | Passed |
| FC-LIFE-INV-017 | INV | **I17**:`SnapshotHandle` 存活期间看到固定 `ReaderView` 的完整视图(段集 + 取快照时的可变表快照);后台 compaction 提交不改变其可见性与正确性(旧视图经 `Arc` 保持,旧段延迟回收)。视图一致性的水位快照部分由 `tests/memory_contracts.rs::seqno_and_rowid_stable_prop` 观测 | `tests/l5_contracts.rs::compaction_preserves_snapshot_and_as_of_within_horizon` | Passed |
| FC-LIFE-INV-023 | INV | **I23**:自动遗忘默认关闭;删除可审计(墓碑在 `history_horizon` 内保留,默认永久,经 `iter_with(..., true)` 可见),绝不静默 | `tests/life_contracts.rs::retain_forgets_below_threshold`、`tests/l5_contracts.rs::auto_retention_is_off_by_default`、`tests/l5_contracts.rs::auto_retention_forgets_expired_records` | Passed |
| FC-LIFE-POST-001 | POST | `retain` 返回 `forgotten` 与 `sampled_ids` 与实际墓碑一致 | `tests/life_contracts.rs::retain_forgets_below_threshold` | Passed |
| FC-LIFE-POST-002 | POST | 保留分公式 `score = importance·2^(−age/T½) + w·ln(1+access_count)`;`age = max(0, now − max(valid_from, last_access))`(`valid_from` 取记录有效时间起,`last_access` 取最近访问;`T½=0` 时衰减项为 0,`age<0` 按 0);`min_importance`/`access_weight` 任一含非有限值 → `Config`(绝不静默永不遗忘) | `src/memory/lifecycle.rs::retention_score_formula`、`tests/life_contracts.rs::error_taxonomy_is_specific` | Passed |
| FC-LIFE-POST-003 | POST | **增量段 flush**:`flush` 把 `seqno > watermark` 的槽位与自上次 flush 的访问/关系 `delta` 物化进**新段**;旧段保持活跃、不改写、不入 trash;`watermark` 推进、WAL Checkpoint;读取面跨段合并后与全量内存状态逐位一致;无新增且无 delta 时为空操作(不产段) | `tests/l5_contracts.rs::incremental_flush_appends_segments`、`tests/l5_contracts.rs::incremental_flush_does_not_rewrite_committed_segments`、`tests/l5_contracts.rs::empty_flush_is_noop` | Passed |
| FC-LIFE-POST-004 | POST | **访问统计攒批**:查询命中把 `RowId` 追加进内存缓冲(同 RowId 按键累加;缓冲条目数上限 `MAX_ACCESS_BUFFER_ENTRIES = 1 << 20`(约 100 万),达到上限后新 `RowId` 丢弃(已有键继续累加),防维护线程停止时无界增长),攒批(默认 `access_flush_interval` = 30s)把缓冲合并为每 RowId 一条 WAL `TouchRow`(`access_delta` = 缓冲累计值 ≥ 1);崩溃最多丢一个攒批周期的访问计数,只影响遗忘速度估计、不影响记录可见性与检索正确性;显式 `touch` 仍即时 WAL 落盘 | `tests/l5_contracts.rs::access_hits_are_batched_and_flushed`、`tests/l5_contracts.rs::access_buffer_cap_discards_new_rowids`(heavy:`#[ignore]`,1M+1 条记录验证上限后丢弃) | Passed |
| FC-LIFE-POST-005 | POST | **命名空间路径规范化**:`namespace(path)` 去首尾 `/`、合并连续 `/`,规范化后为空 = 根命名空间;键唯一性按规范化路径;深度 > `Limits.ns_depth` 或非法字符于**首次写入**时 → `Config`;`list_namespaces` 按规范化路径字典序返回 | `tests/l5_contracts.rs::namespace_paths_are_normalized_and_boundary_matched`、`tests/l5_contracts.rs::namespace_depth_limit_reported_at_first_write` | Passed |
| FC-LIFE-POST-006 | POST | **命名空间注销持久化**:`drop_namespace(path)` 按 `/` 段边界匹配(`a/b` 不含 `a/bc`),墓碑命中记录并移除注册表,返回墓碑**行数**;注销经 WAL 帧持久化,崩溃恢复后已注销路径不再出现;`NsId` 水位不回退、`NsId` 永不复用 | `tests/l5_contracts.rs::drop_namespace_is_durable_across_crash`、`tests/l5_contracts.rs::namespace_paths_are_normalized_and_boundary_matched` | Passed |
| FC-LIFE-POST-007 | POST | `SnapshotHandle::stats()` 返回 `SnapshotStats { version, segments, rows }`:段数/行数取快照钉住视图、`version` 为视图基线序号水位;快照统计不随后续写入/compaction 变化;`SnapshotNamespace::get_many_by_rowid` 与 `Namespace` 读取面一致 | `tests/l5_contracts.rs::snapshot_stats_pin_view` | Passed |
| FC-LIFE-POST-008 | POST | `backup_to` 同盘优先硬链接、失败/跨盘回退逐文件复制;`BackupReport.hardlinked` 如实报告;两条路径产物均满足 `FC-PERSIST-POST-004` 可独立 `open` + `check` | `tests/l5_contracts.rs::backup_hardlinks_segments_when_possible`、`src/persist/store/snapshot/tests.rs::hardlink_failure_falls_back_to_copy` | Passed |
| FC-LIFE-POST-009 | POST | `check()` 报告每段墓碑/逻辑过期占比与建议动作(如「建议合并 N 个段」);占比统计只读元数据(不读向量),不把健康库判为损坏 | `tests/l5_contracts.rs::stats_report_latency_dead_ratio_and_fsck_suggestions` | Passed |
| FC-LIFE-POST-010 | POST | **后台维护可控**(`Builder::maintenance`,默认 `true`):`false` 时不启动后台维护线程——自动 compaction、自动遗忘、访问统计周期落 WAL 均不运行;手动 `Mneme::maintenance_tick()`/`compact()`/`retain()` 不受影响(显式入口照常执行);只读实例的 MANIFEST 探测线程与本开关无关。供批量导入"闸住维护、建完统一整理"(避免导入与维护争抢 CPU/IO) | `tests/l5_contracts.rs::maintenance_can_be_disabled` | Passed |
| FC-LIFE-POST-011 | POST | **单轮 compaction 输入字节预算**(`CompactionPolicy.io_budget`):一次 `Mneme::compact()` 或一次后台维护 tick 在预算内**连续**合并段组——预算 = 活跃段文件(vsec+msec+hidx)总字节 × `io_budget`(排除损坏隔离段,stat 失败按 0 计),每组合并后累计其输入字节,`spent + next > budget` 即结束本轮;`spent == 0` 时无条件执行第一组(至少一个段组,保证有进展;`io_budget = 0` 即每轮一组),余下段组留待下一次;预算只削调度节奏,绝不改变 size-tiered 触发语义与 I8 收敛性(段数界经多次 compact/维护继续成立);`io_budget` 非 `[0,1]` 有限值建库即 `Config`(`FC-LIFE-PRE-001`);轮数上限 = 起始活跃段数(防异常下的无限循环) | `tests/l5_contracts.rs::compaction_respects_io_budget` | Passed |
| FC-LIFE-STA-001 | STA | compaction 五元组 `M=(S={Idle,Running,Paused}, E={Trigger,Step,Pause,Resume,Abort,Done,Fail}, δ(Idle,Trigger)=Running, δ(Running,Pause)=Paused, δ(Paused,Resume)=Running, δ(Running,Done)=Idle, δ(Running,Fail)=Idle, δ(Paused,Abort)=Idle, s0=Idle, F={Idle})`;`stats().compaction` 反映当前态(`Paused` 保留进度与段列表);`pause()` 只在「新段写完、MANIFEST 提交前」这一个步骤边界检查生效,提交后不可中止;非法转移(如 `Idle` 上 `Resume`/`Pause`)不改变状态 | `src/memory/ops.rs::compaction_state_transitions_follow_spec`、`tests/l5_contracts.rs::compaction_respects_pause`、`tests/l5_contracts.rs::pause_during_running_aborts_before_commit`、`tests/l5_contracts.rs::compaction_bounds_segment_count` | Passed |
| FC-LIFE-ERR-001 | ERR | compaction 运行期失败(ENOSPC/I/O/编码错误)→ 以 `Io`/`Corrupted` 上报并回到 `Idle`,已提交 MANIFEST 与数据不变;孤儿新段由下次启动清理;MANIFEST 提交点之后的旧段清理失败**不视为运行期失败**(内存视图先与新 MANIFEST 对齐,旧段成为孤儿、由下次启动清理),绝不静默吞错或半提交 | `tests/l5_contracts.rs::compaction_failure_keeps_state_and_returns_idle`、`tests/l5_contracts.rs::compact_cleanup_failure_after_commit_still_succeeds` | Passed |

---

## 6. 量化与门面(quant)

> `src/quant/` 是纯原语模块(无 I/O、无锁、无全局态;依赖等级同 L0,见 08 §1);
> 量化副本随段同生同灭:flush/compaction 按当前配置写 `vsec` qvec 区,f32 原向量始终保留
> 供精排(两阶段检索)。纯内存库无段,配置量化在构造期返回 `Unsupported`。

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-QUANT-PRE-001 | PRE | `Tuning.rescore_oversample ≥ 1`;`Tuning.quant_recall_floor` 为有限值且 `≥ 0`(`> 1` 表示恒回退,供测试/强制关闭);`quantization != F32` 且未设置 `path`(纯内存库)于构造期返回 `Unsupported`(FC-QUANT-ERR-002);非法值返回 `Config` | `tests/l6_contracts.rs::in_memory_quantization_is_unsupported`、`tests/l6_contracts.rs::invalid_tuning_rejects_quantization_knobs` | Passed |
| FC-QUANT-POST-001 | POST | i8 每维线性量化误差上界 `\|x − x̂\| ≤ Δ/2`(`Δ = (v_max−v_min)/255`;`v_max = v_min` 时 `x̂ = v_max`);编码码值恒在 `[0,255]`;解码与编码使用同一张段级逐维表 | `src/quant/scalar_i8.rs::encode_error_within_half_delta`、`src/quant/scalar_i8.rs::error_bound_holds_for_arbitrary_data_prop` | Passed |
| FC-QUANT-POST-002 | POST | vsec v5 qvec 区:i8 = `2d` 个 f32 `(v_min,v_max)` 交错表(LE) + `row_count × d` 字节码;f16 = `row_count × 2d` 字节;`encode → parse → VsecView` 往返逐位一致,payload CRC 覆盖 qvec 区 | `src/persist/vsec/tests.rs::vsec_i8_quantized_roundtrip`、`tests/l6_contracts.rs::i8_qvec_roundtrip_after_reopen` | Passed |
| FC-QUANT-POST-003 | POST | 开启量化后 flush 与 compaction 均按**当前** `quantization` 写/重写副本(格式迁移零特殊逻辑);f32 原向量始终保留,`Hit.score` 走 f32 精排 | `tests/l6_contracts.rs::compaction_rewrites_quantized_copies` | Passed |
| FC-QUANT-POST-004 | POST | 量化两阶段检索相对 f32 检索的 Recall@10 损失 ≤ 2%(小规模确定性数据;离线 1M×1536 门槛见 14 §4) | `tests/l6_contracts.rs::quantized_two_stage_recall_loss_within_two_percent` | Passed |
| FC-QUANT-INV-012 | INV | **I12**:量化模式 `Hit.score` = f32 精排分 | `tests/l6_contracts.rs::quantized_hit_score_matches_f32_exact` | Passed |
| FC-QUANT-INV-013 | INV | **I13**:建段抽样一致率 < `quant_recall_floor` → 该段自动回退 F32(不写 qvec),`stats().quant` 的 `configured`/`active`/`recall_est` 如实反映 | `tests/l6_contracts.rs::unreachable_recall_floor_falls_back_to_f32`、`tests/l6_contracts.rs::quantized_segment_reports_recall_estimate` | Passed |
| FC-QUANT-INV-014 | INV | **I14**:async 与 sync API 等价(共享同一写锁;同操作序列产生相同状态与错误) | `tests/l6_contracts.rs::async_and_sync_namespace_sequences_are_equivalent` | Passed |
| FC-QUANT-INV-015 | INV | 两阶段粗排候选数 `≤ min(top_k × rescore_oversample, 候选总数)`;精排一律基于 f32 原向量并按 f32 分重排,不复用量化序 | `src/memory/search/tests.rs::coarse_candidates_respect_rescore_cap`、`src/memory/search/tests.rs::rescore_ordering_is_metric_aware_and_total` | Passed |
| FC-QUANT-ERR-001 | ERR | 未开 `quant-f16` 时 `VectorFormat::F16` 构造期返回 `Unsupported { feature: "quant-f16" }`,不静默降级;`F32`/`I8Rescored` 不依赖 feature | `tests/l6_contracts.rs::f16_requires_feature_at_build`、`tests/l6_contracts.rs::f16_roundtrip_when_feature_enabled`、`src/quant/mod.rs::format_support_matches_feature_gate` | Passed |
| FC-QUANT-ERR-002 | ERR | 纯内存库(`Builder` 无 `path`)配置 `F16`/`I8Rescored` → 构造期 `Unsupported`;打开含 f16 段而当前构建未开 `quant-f16` → `Unsupported`,绝不静默按 f32 服务(跨 feature 端到端用例:CI 矩阵先 `--features quant-f16` 建库、默认构建再打开,`MNEME_F16_FIXTURE` 传目录) | `tests/l6_contracts.rs::in_memory_quantization_is_unsupported`、`tests/l6_contracts.rs::write_f16_fixture_for_cross_feature_check`、`tests/l6_contracts.rs::open_f16_fixture_requires_feature`、`src/quant/mod.rs::format_support_matches_feature_gate` | Passed |
| FC-QUANT-ERR-003 | ERR | vsec qvec 区未知 `quant` 编码、长度与头不符、非有限/失序的逐维表 → `Corrupted`,绝不部分解析 | `tests/l6_contracts.rs::qvec_corruption_is_detected`、`src/persist/vsec/tests.rs::vsec_rejects_unknown_quant_code`、`src/persist/vsec/tests.rs::vsec_rejects_malformed_i8_params`、`src/persist/vsec/tests.rs::vsec_rejects_length_mismatch` | Passed |

---

## 7. 安全与部署(security/deploy)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-SEC-INV-028 | INV | **I28**:开启加密后,磁盘上段/WAL/MANIFEST 均为自描述 AEAD 信封(`MNEC` 头 + AES-256-GCM,固定头字段也密文化),不含明文记录字段(key/text/metadata);AAD 绑定 `(用途, 段号/版本, 格式版本)` 防跨文件搬运;翻转密文 1 bit 或使用错误密钥 → `Corrupted`,绝不返回错误数据 | `tests/security_contracts.rs::encrypted_library_never_writes_plaintext`、`tests/security_contracts.rs::tamper_and_wrong_key_are_detected`、`src/crypto/tests.rs::envelope_roundtrip_hides_plaintext`、`src/crypto/tests.rs::tampering_and_wrong_scope_are_rejected` | Passed |
| FC-SEC-POST-001 | POST | **密钥轮换**(`Mneme::rotate_encryption_key`):`provider.rotate()` 生成新 active 密钥后,以"全部活跃段"为计划强制 compaction 全量重写段与 MANIFEST(新段用新密钥,旧段入 `trash/`);迁移期间新旧密钥均可读,迁移完成后 `Keyring::retire` 退役旧密钥仍可完整打开;`stats().storage.migrated_segments` 以信封头 `key_id` 是否等于 active 实计(统计只读 vsec 信封头 10 字节,不整读段文件);`Keyring::rotate` 密钥编号分配 checked,耗尽 → `IdExhausted { kind: "key_id" }`,绝不静默复用同编号覆盖已有密钥 | `tests/security_contracts.rs::key_rotation_migrates_and_retires_old_key`、`tests/security_contracts.rs::migrated_segments_probes_envelope_header_without_full_read`、`src/crypto/tests.rs::keyring_rotate_exhaustion_returns_id_exhausted` | Passed |
| FC-DEPLOY-INV-029 | INV | **I29**:只读实例看到的始终是某已提交 MANIFEST 版本的完整视图(段集/版本链/索引自洽);打开后不随写者刷新,周期探测(`read_only_probe_interval`,默认 1s)发现新提交版本时经 [FC-DEPLOY-STA-001] 原子切换;写者崩溃后只读实例仍可读最后一个已提交版本 | `tests/deploy_contracts.rs::read_only_instances_see_committed_views_atomically` | Passed |
| FC-DEPLOY-INV-030 | INV | **I30**:`Observer` 回调不改变引擎行为;事件字段与实际操作一致;回调 panic 被 `catch_unwind` 隔离(读写仍成功、状态不变);未注册时零事件构造 | `tests/deploy_contracts.rs::observer_events_fire_and_panics_are_isolated`、`src/core/observe.rs::emit_delivers_and_isolates_panics` | Passed |
| FC-DEPLOY-STA-001 | STA | 只读视图切换:`V_n → V_{n+1}` 原子(`Table::publish` 换 `Arc<ReaderView>`);不存在中间态,旧视图由持引用者继续使用;无新版本时 `reload()` 返回 `None` 不重建 | `tests/deploy_contracts.rs::read_only_instances_see_committed_views_atomically`、`tests/deploy_contracts.rs::repeated_reload_is_idempotent` | Passed |
| FC-DEPLOY-POST-001 | POST | **存储后端抽象**(`Storage` + `Builder::storage`):段/WAL/MANIFEST/trash/锁全部经注入后端读写;默认 `FsStorage`;`MemStorage` 支持完整生命周期(建库/写入/flush/重开/检索/check),不接触进程文件系统;路径穿越被拒绝;后端专属锁语义(`try_lock`)保证单写者 | `tests/deploy_contracts.rs::custom_storage_backend_supports_full_lifecycle`、`src/persist/storage/tests.rs::resolve_rejects_traversal`、`src/persist/store/wal_writer/tests.rs::mem_storage_supports_wal_lifecycle` | Passed |

---

## 8. 边界与极值(全局)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-GLOBAL-PRE-001 | PRE | 向量维度 ∈ [1, 65536];长度 ≠ 建库维度 → `DimensionMismatch`(写/查同口径) | `tests/memory_contracts.rs::insert_rejects_dimension_and_non_finite`、`tests/query_contracts.rs::search_rejects_dimension_mismatch` | Passed |
| FC-GLOBAL-PRE-002 | PRE | 任一分量 `NaN`/`±Inf` → `NonFinite`,库内不被污染 | `tests/memory_contracts.rs::insert_rejects_dimension_and_non_finite` | Passed |
| FC-GLOBAL-PRE-003 | PRE | key ≤ 1024B、text ≤ 1MiB、meta ≤ 64KiB、深度 ≤ 32 → 否则 `TooLarge`/`MetaTooDeep` | `tests/memory_contracts.rs::write_limits_reject_too_large`、`tests/memory_contracts.rs::write_meta_limits_reject_too_large`、`tests/memory_contracts.rs::write_limits_boundary_three_point`、`tests/memory_contracts.rs::update_enforces_write_limits` | Passed |
| FC-GLOBAL-PRE-004 | PRE | `importance`/`confidence` 越界钳制到 [0,1],含非有限值(NaN)→ `NonFinite` 拒绝(含 `UpdatePatch` 与 `touch` boost、关系边权);`top_k`/`ef` > 4096 → `LimitExceeded`;`dedup_threshold`/`threshold`/`min_importance`/`access_weight` 等策略参数与 MMR `lambda` 含非有限值 → `Config`;`dedup_threshold`/`threshold` 越界 [0,1] → `Config` | `tests/memory_contracts.rs::importance_and_confidence_clamped`、`tests/memory_contracts.rs::importance_confidence_boundary_three_point`、`tests/query_contracts.rs::search_limits_reject_top_k_and_ef`、`tests/life_contracts.rs::error_taxonomy_is_specific`、`tests/model_contracts.rs::relate_weight_rejects_non_finite_and_clamps` | Passed |
| FC-GLOBAL-ERR-001 | ERR | 库绝不 panic;文档化例外共三类:① `filter!` 字面量(`src/lib.rs`);② async `spawn_blocking`(async 门面,feature `async`);③ 槽位下标 `u32::try_from(..).expect` 六处(`src/memory/search/entry.rs` 的 `collect_candidates` 候选收集、`src/memory/table/state/index.rs` 的 `rebuild_indexes`/`install_segment`、`src/memory/table/state/version.rs` 的 `prune_reclaimed`、`src/persist/flush/index.rs` 的 `build_index`、`src/query/plan/compile.rs` 的 `compile`),均由 `FC-MEM-INV-004`(槽位下标 ≤ `u32::MAX`)保证不可达;另有 `src/persist/wal/codec.rs` 的 `encode_frame` 负载长度转换(单帧负载远小于 `u32::MAX`,`FC-GLOBAL-PRE-003` 限额)一并登记为文档化例外 | `tests/life_contracts.rs::l1_api_smoke_never_panics` | Passed |
| FC-GLOBAL-ERR-002 | ERR | 错误分类矩阵(§0.2)各变体语义互不混淆:`Closed`/`NonFinite`/`LimitExceeded`/`MetaTooDeep`/`Config`/`Unsupported`/`Inconsistent` 各由专属条件触发 | `tests/life_contracts.rs::error_taxonomy_is_specific` | Passed |
| FC-GLOBAL-PRE-005 | PRE | 时钟回拨经单调水位钳制(取历史最大值);记录不会因回拨早消失/复活 | `tests/memory_contracts.rs::clock_rollback_does_not_resurrect_expired_record` | Passed |
| FC-GLOBAL-INV-001 | INV | **配置一律显式**:库本体不读取环境变量(源码不含 `std::env`/`env::var` 访问),所有调参与开关经 `Builder`/`Tuning`/`Limits` 等显式注入;dev 侧(契约测试/示例)环境变量读取统一收敛于 `tests/common/env.rs`(示例经 `#[path]` 复用同一文件,空值/非法值口径一致) | `tests/deploy_contracts.rs::library_source_never_reads_environment_variables` | Passed |

---

## 9. 算法复杂度契约(CPLX)

> 复杂度是**资源后置约束**:每个公开操作在声明的输入规模下,时间与空间开销必须落在
> 下表上界内。所有复杂度以**最坏情况**为默认口径;显式标注「摊还」「期望」者除外。
> 变量定义见 §9.1,验证方式与门禁见 §9.3;与 [15 §3 复杂度速查](../design/15-glossary.md)
> 对应(后者是阅读视图,覆盖主流操作;本节是唯一真实数据源,冲突以本节为准)。

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
| $\text{seeds}$ | 联想扩展的上跳种子数 |
| $\text{avg\_degree}$ | 关系图平均度数 |
| $\text{max\_nodes}$ | 联想扩展的访问上限(`visited` + 结果,`RelationExpand::max_nodes`) |
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
| FC-CORE-CPLX-001 | CPLX | `simd::dot` / `dot_scalar` / `Metric::score`:时间 $O(d)$;指令数依运行时分发,AVX-512F $\approx 3d/16+3$、AVX2 $\approx 3d/8+3$、SSE2/NEON $\approx 3d/4+3$;空间 $O(1)$ | 操作计数单测 `src/core/simd/tests.rs::dot_scalar_per_element_cost_is_linear`(逐元素乘加计数 == `d`)+ 解析证明(02 §3.4/§4.3:单遍 $d$ 次乘加,每向量内核 1 FMA + 2 加载) | Passed |
| FC-CORE-CPLX-002 | CPLX | `Metric::better` / `needs_norm`:时间 $O(1)$、空间 $O(1)$ | 解析证明(02 §3.4:两者为常数分支,不含循环)+ 哨兵 `tests/core_contracts.rs::metric_better_direction`、`tests/core_contracts.rs::metric_needs_norm` | Passed |
| FC-CORE-CPLX-003 | CPLX | `TopK::push`:未满 $O(\log k)$、已满 $O(1)$ 拒绝或 $O(\log k)$ 下沉(最坏 $O(\log k)$);`TopK::new` 预分配 $\le \min(k,1024)$;空间 $O(k)$ | 操作计数单测 `src/core/heap/tests.rs::topk_prealloc_bounded_by_min_k_1024`、`src/core/heap/tests.rs::topk_push_outside_k_costs_constant_or_log_k`(拒绝路径恰 1 次比较)+ 解析证明(02 §5.2/§5.4) | Passed |
| FC-CORE-CPLX-004 | CPLX | `TopK::merge` / `into_sorted_vec`:时间 $O(k\log k)$,**与 $N$ 无关**;空间 $O(k)$ | 操作计数单测 `src/core/heap/tests.rs::topk_merge_and_sort_cost_bounded_by_k`(只操作 $\le k$ 个元素,比较次数与扫描规模 $N$ 无关且以 $k\log k$ 为界)+ 解析证明(02 §5.4) | Passed |
| FC-CORE-CPLX-005 | CPLX | `varint::{encode_u32,encode_u64,decode_u32,decode_u64}`:时间 $O(\lfloor\log_{128}x\rfloor+1)\le 10$ 字节操作;空间 $\le 10$ B | 最小编码/10 字节上界断言 `tests/core_contracts.rs::varint_malformed`、`tests/core_contracts.rs::varint_roundtrip_minimal` | Passed |
| FC-CORE-CPLX-006 | CPLX | `meta::get_path`:时间 $O(p)$($p$ = `.` 分段数,每段平均 $O(1)$ 查找);`as_f64/as_i64/as_bool/as_str/as_ts`: $O(1)$ | 解析证明(02 §7:按 `.` 分段逐级下降,每级一次 `Value::get`;访问器为单次类型匹配)+ 哨兵 `tests/core_contracts.rs::meta_accessors` | Passed |

#### 9.2.2 L1 内存层(mem)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-MEM-CPLX-001 | CPLX | 暴力扫描:时间 $O(N\cdot d)$,过滤后 $O(N_c\cdot d)$;空间 $O(N/8)$ 位图 + $O(k)$ | 操作计数单测 `src/memory/search/tests.rs::scan_touches_each_candidate_once`(打分次数 = 候选数)+ 语义哨兵 `tests/query_contracts.rs::brute_force_matches_reference`、PBT `tests/query_contracts.rs::brute_force_matches_reference_prop` | Passed |
| FC-MEM-CPLX-002 | CPLX | 元数据过滤求值:时间 $O(N\cdot\|\phi\|)$,单行 $O(\|\phi\|)$(短路求值);空间 $O(N/8)$ | 解析证明(设计 03 §5.2)+ 哨兵 `tests/query_contracts.rs::filter_uses_kleene_three_valued_logic` | Passed |
| FC-MEM-CPLX-003 | CPLX | 并行归并:时间 $O(C_{\text{block}}\cdot k\log k)$;空间 $O(C_{\text{block}}\cdot k)$ | 解析证明(设计 03 §4.3)+ 哨兵 `tests/query_contracts.rs::search_order_is_total_and_stable` | Passed |
| FC-MEM-CPLX-004 | CPLX | `delete` / `touch`:时间 $O(\log n)$ 定位 + $O(1)$ 墓碑/统计更新;空间 $O(1)$ | 解析证明(HashMap 定位 + 版本链追加)+ 哨兵 `tests/memory_contracts.rs::delete_hides_records_from_reads` | Passed |
| FC-MEM-CPLX-005 | CPLX | `iter(filter)`:时间 $O(N)$ 全量元数据扫描(遍历 `view.latest` 逐条按 `ns_id` 过滤,实现不按命名空间分桶)+ $O(N_c\cdot\|\phi\|)$ 谓词求值 + $O(N_c\log N_c)$ RowId 排序;空间 $O(N_c)$(物化命中行的 `Arc` 句柄列表,**不复制记录体**;调用方收集的结果集另计) | 过滤/排序/墓碑哨兵 `tests/query_contracts.rs::iter_filters_and_sorts_by_rowid` | Passed |

#### 9.2.3 L2 持久层(persist)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-PERSIST-CPLX-001 | CPLX | WAL 提交:单条 $O(1)$ 内存追加;组提交 $N$ 条 $O(N)$ 追加 + **1 次** fsync/批;空间顺序写 | 操作计数单测 `tests/persist_contracts.rs::batch_insert_uses_single_fsync`(整批 Fsync 动作数 = 1) | Passed |
| FC-PERSIST-CPLX-002 | CPLX | WAL 回放(`wal::visit_frames`):时间 $O(\text{有效帧})$;空间 $O(1)$ 流式(批内帧缓冲 ≤ 单批帧数)。注:`Store::open` 仍把单个 WAL 文件整文件读入内存后流式回放(单文件受 `wal_file_bytes` 上界约束,默认 64MB,见 FC-PERSIST-POST-011);段文件经惰性段驻留不整段读入(FC-PERSIST-INV-021) | 解析证明(设计 04 §3.4;`wal::visit_frames` 逐帧回调不物化)+ 哨兵 `src/persist/wal/mod.rs::wal_replay_roundtrip` | Passed |
| FC-PERSIST-CPLX-003 | CPLX | CRC-32:时间 $O(n)$、空间 $O(1)$;$n$ = 字节数(实现为 `crc32fast` 查表/切片,常量因子依平台) | 解析证明(设计 04 §4.4;`crc32fast`)+ 哨兵 `src/persist/vsec/tests.rs::vsec_detects_payload_corruption` | Passed |
| FC-PERSIST-CPLX-004 | CPLX | zone map:**写路径增量维护**(每条记录每字段均摊 $O(1)$、共 $O(N\cdot\text{fields})$,随快照以 `Arc` COW 共享;存在长命快照句柄时单次写可能深拷贝索引,属已登记取舍),flush 仅编码 $O(\text{blocks}\cdot\text{fields})$;查询期块级剪枝 $O(\lceil n/1024\rceil \times \text{predicates})$;落盘空间 17 B/块/字段(`has_any`/`mixed` 标志仅存内存,重开时 zone map 从槽位重建)。写入 msec `zmap` 区并服务于查询计划器 | 解析证明(设计 04 §5.2)+ 哨兵 `tests/l4_contracts.rs::plan_filter_matches_pointwise_count` | Passed |
| FC-PERSIST-CPLX-005 | CPLX | bloom:构建 $O(N_{key}\cdot k)$、判定 $O(k)=O(7)$;空间 $1.44\log_2(1/p)$ bit/元素(**承诺在元素数 ≤ 初始容量 65536 时成立**;写路径超出后位图饱和,只升误报率、绝不漏报)。极小/非法 `fpp` 在校验与构造两层夹紧,`k` 恒落在可落盘范围 `[1,64]`。写入 msec `bloom` 区,供 `key` 等值预筛 | 解析证明(设计 04 §5.3 双哈希)+ 哨兵 `tests/l4_contracts.rs::text_index_survives_reopen`、`src/memory/analysis/bloom.rs::extreme_fpp_stays_within_storable_k` | Passed |
| FC-PERSIST-CPLX-006 | CPLX | MANIFEST 提交:时间 $O(S_{\text{seg}})$ 写新文件;空间保留 2 版 | 解析证明(设计 04 §6;`manifest::encode` 逐段线性)+ 哨兵 `src/persist/manifest/tests.rs::manifest_roundtrip` | Passed |
| FC-PERSIST-CPLX-007 | CPLX | `open` 恢复(feature `mmap`,默认):时间 $O(\text{段数} + \text{版本行数} + \text{hidx 头/node\_table} + \text{hidx 整文件 CRC} + \text{WAL 字节} + \text{msec 元数据区})$(向量/量化码/图邻接**不物化**,按需缺页或按需读;关闭 feature `mmap` 时回退整段读入自有缓冲——功能等价,不在性能承诺内);空间 $O(\text{元数据} + \text{版本链} + \text{node\_table} + \text{mmap 映射})$,不随向量区字节线性驻留(段句柄惰性驻留,FC-PERSIST-INV-021);`verify_on_open` 开启时额外 $O(\text{段总字节})$ CRC 校验(显式诊断口径,默认关,不改变功能语义) | 哨兵 `tests/persist_contracts.rs::reopen_after_drop_recovers_from_wal`、`tests/persist_contracts.rs::lazy_reopen_matches_bruteforce`、`tests/persist_contracts.rs::segment_open_probes_prefix_without_full_read`;解析证明(设计 04 §7/§8) | Passed |
| FC-PERSIST-CPLX-008 | CPLX | 单点写 `insert`:时间 $O(1)$ 内存 + WAL 追加(fsync 按 `FsyncPolicy`);空间 $O(d)$ | 哨兵 `tests/persist_contracts.rs::reopen_after_close_recovers_records`;解析证明(HashMap 追加 + 定长帧) | Passed |
| FC-PERSIST-CPLX-009 | CPLX | 单点读 `get(key)`:时间 $O(\log n)$ + 一次记录读(实现为 HashMap 期望 $O(1)$ $\subseteq O(\log n)$);`get_by_rowid`: $O(\log n)$ 版本链定位 | 哨兵 `tests/persist_contracts.rs::namespace_and_rowid_survive_reopen`;解析证明(设计 04 §5.5) | Passed |
| FC-PERSIST-CPLX-010 | CPLX | `as_of(t)`:时间 $O(V + N_c\cdot d)$($V$ = 全部物理版本数,`temporal::snapshot_at` 单遍择版本;实现不随段数$S_{\text{seg}}$分层加速);空间 $O(V + N_c)$(随历史窗口与候选规模增长) | 哨兵 `tests/persist_contracts.rs::version_chain_survives_reopen`;解析证明(设计 04 §5.5) | Passed |
| FC-PERSIST-CPLX-011 | CPLX | 增量 flush:时间 $O(\Delta + D)$($\Delta$ = 未物化槽位数,$D$ = 自上次 flush 的访问/关系变更数),不重写已提交段;空间额外 $O(\Delta + D)$ | 解析证明(`Store::flush` 位于 `src/persist/store/snapshot/`,仅编码 `unpersisted_slots()` 与 `build_delta`,不读旧段)+ 哨兵 `tests/l5_contracts.rs::incremental_flush_does_not_rewrite_committed_segments`、`tests/l5_contracts.rs::empty_flush_is_noop` | Passed |

#### 9.2.4 L3 索引层(index)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-INDEX-CPLX-001 | CPLX | HNSW 单点插入:时间 $O(d\cdot(ef_c\cdot M_0 + M\log_M N))$;构建 $O(N\cdot d\cdot ef_c\cdot M_0)$;空间 $\approx(8M+20)$ B/节点 + 边表。注:$M$/$M_0$ 为**有界常数**(≤4096,FC-INDEX-PRE-001);启发式选邻与修剪的 $M_0$ 多项式项(变量展开时最坏额外 $O(d\cdot M_0^3\cdot\log_M N)$)在最坏口径下含于系数,不影响 $N$/$d$/$\log N$ 的渐进结论。`Hybrid` 档(FC-INDEX-POST-010)距离计算次数同上界不变,单次距离的读带宽常数降低(i8 码流 d 字节对 f32 的 4d 字节) | 操作计数单测 `src/index/hnsw/tests.rs::build_distance_calls_scale_linearly` | Passed |
| FC-INDEX-CPLX-002 | CPLX | HNSW 查询:期望上界 $O(d\cdot ef\cdot M_0)$(实测 $\approx(2\text{–}5)\cdot ef$ 次点积);空间期望 $O(ef\cdot M_0)$(`visited` 集合覆盖已展开节点的邻边)。注:本条目只约束索引内部;调用方 `memory::search` 的候选收集与 alive/过滤位图构造为 $O(N)$(已声明的内存元数据扫描;向量与图邻接字节经段句柄惰性读取,不随扫描复制,FC-PERSIST-INV-021;设计 05 §8) | 操作计数单测 `src/index/hnsw/tests.rs::search_distance_calls_bounded_by_ef` | Passed |
| FC-INDEX-CPLX-003 | CPLX | 上层下降:时间期望 $O(d\cdot M\cdot\log_M N)$(依赖层级随机分布,§9.1 口径③);层高期望 $O(\log_M N)$ | 操作计数单测 `src/index/hnsw/tests.rs::level_height_grows_logarithmically` | Passed |
| FC-INDEX-CPLX-004 | CPLX | hidx 编解码:时间 $O(\text{nodes}+\text{edges})$、空间 $O(\text{bytes})$ | 操作计数单测 `src/index/hidx/tests.rs::hidx_encode_decode_scale_linearly` | Passed |

#### 9.2.5 L4 检索与排序层(query/score)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-QUERY-CPLX-001 | CPLX | DSL 解析:时间 $O(L)$ 单遍;空间 $O(\|\phi\|)$ | 解析证明(设计 06 §1.2:单遍递归下降)+ 哨兵 `tests/l4_contracts.rs::dsl_parse_handles_large_input_once`(2000 项 Or 链) | Passed |
| FC-QUERY-CPLX-002 | CPLX | 计划编译:时间 $O(N + \text{blocks}\times\text{predicates})$、空间 $O(n/8)$ 块位图 + $O(N_c)$ 候选。注:$N$ 为视图物理槽位数(逐行判定命名空间/可见性);块掩码求值本身为 $O(\text{blocks}\times\text{predicates})$。该 $O(N)$ 项为已声明的内存元数据扫描,段字节经惰性句柄读取(FC-PERSIST-INV-021) | 解析证明(设计 06 §2)+ 哨兵 `tests/l4_contracts.rs::plan_filter_matches_pointwise_count` | Passed |
| FC-QUERY-CPLX-003 | CPLX | BM25 打分:时间 $O(N_{ns} + 2\sum_{t\in Q} df_t)$ postings 访问;空间 $O(\min(N_{ns}, \sum_{t\in Q} df_t))$(第二遍物化全部命中文档的分数映射,TopK 另计 $O(k)$)($N_{ns}$ = 查询命名空间有文本的物理槽位数,用于可见性过滤后的 N/avgdl 统计;倒排已按视图物化,段字节经惰性句柄读取,FC-PERSIST-INV-021) | 解析证明(设计 06 §3.2 两遍法)+ 哨兵 `tests/l4_contracts.rs::bm25_formula_behaviour` | Passed |
| FC-QUERY-CPLX-004 | CPLX | RRF/加权融合:时间 $O(k)$、空间 $O(k)$ | 解析证明(设计 06 §4:只对两通道 top-k 名次表操作)+ 哨兵 `tests/l4_contracts.rs::hybrid_fusion_and_validation` | Passed |
| FC-SCORE-CPLX-001 | CPLX | 综合重排/归一化:时间 $O(m\log m)$($m$ = 候选数;逐候选 $O(1)$ 因子计算 + 一次排序);空间 $O(m)$ | 解析证明(设计 10 §2.5:单遍逐候选计算因子与加权和,末尾按分数稳定排序)+ 哨兵 `tests/query_contracts.rs::scoring_composite_factors_clamped` | Passed |
| FC-SCORE-CPLX-002 | CPLX | 联想扩展:时间 $O(\text{seeds}\cdot\text{max\_nodes}\cdot\text{avg\_degree})$($\text{seeds}$ = 上跳种子数;有界 BFS,$hops\le 3$);空间 $O(\text{max\_nodes}+\text{seeds})$(`visited` 集合**总量**受 `max_nodes` 封顶,种子预置其中、结果为其子集;被命名空间/存活/过滤拒绝的节点同样计入,达到上限即提前停止扩展) | 解析证明(设计 10 §3.2:逐跳 BFS,`visited` 总量封顶)+ 哨兵 `tests/query_contracts.rs::expansion_does_not_cross_namespaces` | Passed |
| FC-SCORE-CPLX-003 | CPLX | MMR 贪心:时间 $O(m\cdot k\cdot d)$($m$ = 候选数,$k$ = 返回条数;维护各候选与已选集的最大余弦 `max_sim`,**每个候选-已选对至多计算一次**增量更新,自范数预计算复用);空间 $O(m)$(`max_sim` + 范数表) | 解析证明(设计 10 §5.1:每轮单遍剩余候选,总对级计算 $\le k\cdot m$)+ 操作计数单测 `src/memory/score/tests.rs::mmr_caches_pairwise_similarity` + 哨兵 `tests/query_contracts.rs::scoring_composite_factors_clamped` | Passed |

#### 9.2.6 L5 生命周期层(life)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-LIFE-CPLX-001 | CPLX | TTL 逻辑过期:时间 $O(\text{blocks})$(块级 `min(expires_at)` 剪枝,整块全未过期时不逐行判定);空间 8 B/块(`ttl_map` 落于 msec zmap 区尾,见 FC-PERSIST-POST-008) | `src/query/plan/tests.rs::ttl_unexpired_block_skips_per_row_checks`、`src/persist/msec/index/tests.rs::ttl_map_roundtrip_and_invalid_tail`、`tests/l5_contracts.rs::ttl_expiry_survives_multi_segment_reopen` | Passed |
| FC-LIFE-CPLX-002 | CPLX | `retain` 扫描:时间 $O(N_{\text{cand}})$(元数据级,不读向量);空间 $O(N_{\text{cand}})$ | 解析证明(07 §3:单遍元数据扫描,不读向量、不重建索引)+ 哨兵 `tests/life_contracts.rs::retain_forgets_below_threshold` | Passed |
| FC-LIFE-CPLX-003 | CPLX | compaction 单轮:时间 $O(S_{\text{merge}}\cdot d\cdot ef_c\cdot M_0)$(重建历史版本建图主导;墓碑/过期/horizon 超期过滤与 delta 合并为线性项);摊还 $O(d\cdot ef_c\cdot M_0\cdot W_{\text{amp}})$;空间峰值 $+O(S_{\text{merge}})$ | 解析证明(07 §4.5:逐行过滤/重写线性,建图主导)+ 哨兵 `tests/l5_contracts.rs::compaction_bounds_segment_count` | Passed |
| FC-LIFE-CPLX-004 | CPLX | 活跃段数 $\le (T-1)\log_r(N/B)+c = O(\log_r N)$(I8);WAL $\le wal\_bytes$(轮转见 FC-PERSIST-POST-011) | 哨兵 `tests/l5_contracts.rs::compaction_bounds_segment_count`、`src/life/compact/tests.rs::plan_merges_smallest_same_level_segments`、`src/life/compact/tests.rs::plan_keeps_levels_separate_and_below_threshold_quiet` + 解析证明(07 §4.2) | Passed |
| FC-LIFE-CPLX-005 | CPLX | `snapshot`:时间 $O(1)$(clone `Arc` 视图);`backup_to`:同盘 $O(\text{files})$(硬链接)、跨盘 $O(\text{bytes})$;`check`: $O(\text{total bytes})$ | 解析证明(03 §4.3:快照 clone `Arc` 视图,与数据量无关;backup/check 逐文件/逐字节遍历)+ 哨兵 `tests/persist_contracts.rs::backup_is_independently_openable`、`tests/persist_contracts.rs::check_detects_corrupt_segment` | Passed |
| FC-LIFE-CPLX-006 | CPLX | 后台维护单轮(与 `Mneme::maintenance_tick()` 手动执行同逻辑):access 攒批落盘 $O(\Delta_{\text{access}})$、retain 扫描 $O(N_{\text{cand}})$、compaction 触发检查 $O(S_{\text{seg}})$;单轮不引入未声明的 $O(N)$ 扫描 | `tests/l5_contracts.rs::access_hits_are_batched_and_flushed`、`tests/l5_contracts.rs::auto_retention_forgets_expired_records`、`tests/l5_contracts.rs::auto_compaction_triggers_in_background` + 解析证明(维护循环逐项按上述规模访问) | Passed |

#### 9.2.7 L6 量化层(quant)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-QUANT-CPLX-001 | CPLX | i8 量化点积:粗排副本带宽 $4d\to d$ B/行(÷4);空间副本 $d$ B/行(f32 原向量另存),段级逐维参数表 $2d$ 个 f32(f16 无表、每行 $2d$ B) | 解析证明(08 §2)+ 哨兵 `src/persist/vsec/tests.rs::vsec_i8_quantized_roundtrip`(逐行码流恰 $d$ 字节、参数表恰 $2d$ 个 f32) | Passed |
| FC-QUANT-CPLX-002 | CPLX | 两阶段检索:粗排候选 $\le$ `top_k × rescore_oversample`(默认 4×k,k=10 时 40 个);精排时间 $O(c\cdot d)$($c \le$ 候选上限)、按 f32 分重排后取 $k$ | 解析证明(08 §4.2)+ 哨兵 `src/memory/search/tests.rs::coarse_candidates_respect_rescore_cap` | Passed |

#### 9.2.8 记忆模型 / 安全 / 部署

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-MODEL-CPLX-001 | CPLX | `neighbors(from)`:时间 $O(\log E + degree)$;`predecessors(to)`:默认 $O(E)$ 全段扫描,`RelationIndex::Both` 时 $O(\log E + degree)$;空间 $O(degree)$。注:L1 内存实现为 `HashMap<RowId, Vec<Edge>>`,`neighbors` 期望 $O(1)$+degree(不劣于本条上界)、`predecessors` 全表 $O(E)$;反向表落盘见 `FC-MODEL-POST-007`(msec 关系区,`RelationIndex::Both`) | `tests/model_contracts.rs::predecessors_returns_incoming_edges` | Passed |
| FC-MODEL-CPLX-002 | CPLX | `consolidate` 聚类(`score::cluster_by_similarity`):时间 $O(n^2\cdot d)$(候选 $n$ 两两余弦,并查集近似线性);空间 $O(n)$(并查集 + 分组)。候选规模受单库内存与 `ConsolidationPolicy.filter` 约束;渐进劣化须先改本条(FC-GLOBAL-CPLX-001) | 解析证明(两两比较循环可数)+ 哨兵 `tests/model_contracts.rs::consolidate_merges_cluster_and_keeps_sources` | Passed |
| FC-SEC-POST-002 | POST | **文本/元数据压缩**(feature `compress`):作用于记录体 `text`/`meta`/`provenance` 字段,按字段独立压缩、自描述 codec id 并带 `uncompressed_len`;压缩无收益(不短于原文)时自动存原文;`Compression::None` 时字段布局与未压缩定义一致(仅多一个恒 0 的 `flags2` 字节,`FORMAT_VERSION = 0x0006`);重开后文本/元数据逐字节一致,BM25 与元数据过滤不受影响 | `tests/security_contracts.rs::compression_roundtrip_and_threshold`、`src/compress/mod.rs::lz4_roundtrip_various_inputs`、`src/compress/mod.rs::incompressible_field_falls_back_to_raw` | Passed |
| FC-SEC-ERR-001 | ERR | 安全能力门控与畸形拒绝:feature `compress`/`compress-zstd`/`encrypt` 未开启时配置压缩/加密于构造期 `Unsupported`(绝不静默明文/不压缩落盘);未开 `encrypt` 的构建打开加密库:`crypto::decrypt_file` 的信封探测返回结构化 `Unsupported { feature: "encrypt" }`,MANIFEST/段/WAL 各载入路径均原样上抛该根因(绝不归入 `Corrupted`、绝不按明文解析、绝不静默降级;跨 feature 用例锁定);未知压缩 codec id / 信封版本不符 / 压缩流畸形 → `Corrupted`,绝不 panic | `tests/security_contracts.rs::compression_requires_feature`、`tests/security_contracts.rs::zstd_requires_feature`、`tests/security_contracts.rs::encryption_requires_feature`、`tests/security_contracts.rs::write_encrypted_fixture_for_cross_feature_check`、`tests/security_contracts.rs::open_encrypted_fixture_requires_feature`、`src/compress/mod.rs::lz4_decompress_rejects_malformed_or_length_mismatch` | Passed |
| FC-SEC-CPLX-001 | CPLX | 加解密:时间 $O(n)$(AES-NI;整文件单遍)、空间 $O(n)$;压缩/解压:时间 $O(n)$(LZ4 贪心单遍)、空间 $O(n)$ | 解析证明(设计 11 §2.5/§3.4 单遍)+ 哨兵 `tests/security_contracts.rs::large_payload_roundtrip_is_linear_path` | Passed |
| FC-DEPLOY-CPLX-001 | CPLX | 只读视图切换:时间 $O(1)$(原子交换已构建视图;新视图构建成本与冷启动同类,按探测周期摊销),无隐藏全量扫描 | 解析证明(设计 12 §2.4;`Table::publish` 仅换 `Arc`)+ 哨兵 `tests/deploy_contracts.rs::repeated_reload_is_idempotent` | Passed |

#### 9.2.9 全局(global)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-GLOBAL-CPLX-001 | CPLX | **无隐藏复杂度**:任何公开操作的实际开销不得渐进劣于其声明上界;路径中不得出现未声明的 $O(N)$ 全量扫描 | 复杂度操作计数由手动 `cargo test --release` 运行全部 `FC-*-CPLX-*` 单测(CI 只跑 fast/middle,不上重任务)+ heavy 规模门槛(`tests/heavy_gate.rs::heavy_perf_gates` 建库 ≥300 向量/秒、i8 ef=128 P50 < 50ms / P99 < 100ms,`tests/cold_start.rs`;门槛仅在正式规模 1M×1536 断言,手动回归建议 100_000×128、正式规模待专用 runner);渐进退化须先改契约 | Planned |

### 9.3 验证方式与回归门禁

1. **验证方式三选一**:每条 CPLX 必须由下列至少一种方式验证,并在「对应测试 / 基准」列登记:
   - **操作计数单测**:对结构化操作(堆、位图、postings、版本链)统计比较/访问次数,断言其关于规模的增长率;
   - **criterion 基准门槛**:对吞吐/延迟敏感路径,以 [14 §4](../design/14-testing.md) 门槛为准
     (如 1M×1536 i8 ef=128 P50 < 50ms / P99 < 100ms、4 核构建 ≥ 300 向量/秒);
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
6. **基准与操作计数口径**:criterion 基准覆盖吞吐/延迟敏感路径([14 §4](../design/14-testing.md));L1 的 CPLX
   以「操作计数单测 + 解析证明」验证,基准仅作回归哨兵。标记 `Passed` 的前提是
   验证方式已在「对应测试 / 基准」列显式登记,禁止用纯语义正确性测试冒充复杂度验证。

---

## 10. 追溯规则(FSVDD 强制)

1. **无孤儿实现**:任何新增业务逻辑必须在本矩阵登记至少一条约束;
2. **无失效契约**:本矩阵条目若与代码不符,以"先改契约、再改测试、再改代码"为准;
3. **无孤立测试**:每个测试文件头部注释必须引用其覆盖的 `FC-*` 编号;
4. **100% 映射**:本矩阵条目与其登记的测试一一对应,由 `tests/contract_traceability.rs`
   在 `cargo test` 中机械校验(无悬空引用、无孤立测试);
5. **豁免**:纯文档改动不新增契约;但涉及磁盘格式/API 语义的文档改动必须先更新本矩阵;
6. **证伪原则**:每条 ERR / INV 契约均配备专项失败测试——放宽任一约束
   (如 varint 超长校验、余弦 ε 阈值、`Dimension` 边界)必有一个测试变红;
   机械化变异测试(`cargo-mutants`)由本机/专用 runner 手动执行(见 [14 §7](../design/14-testing.md));
7. **复杂度可追溯**:每条 `CPLX` 契约必须有对应的基准 / 操作计数测试(§9.3);
   任何使复杂度渐进退化的改动,必须先更新 `CPLX` 契约,再改测试与代码。
