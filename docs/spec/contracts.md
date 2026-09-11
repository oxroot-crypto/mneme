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
| 2026-09 | 合并前独立审查整改(第二轮):① `FC-INDEX-POST-001` 明确档③仅在有过滤位图时生效(无过滤时候选即 alive、`s=1` 走档①),消除契约与实现/单测的口径漂移;② `FC-INDEX-CPLX-001/003` 补 $M_0$ 有界常数口径与时间项「期望」标注(§9.1 口径③);③ `FC-INDEX-ERR-001` 补「hidx 节点数与恢复槽位数不一致 → `Corrupted`」失败分支与定向测试,并补截断/头 CRC/布局定向用例、`decode` 任意字节 proptest 强化为「接受即往返」;`FC-PERSIST-ERR-009` 补载入期重排映射越界二次校验与测试;④ `FC-INDEX-POST-005` 补 ANN 路径 NS/TTL 可见性验收;⑤ Rust 规范整改:`ann_search` 参数收敛为预算结构体、`parse_header` 拆出参数校验、`simd` 操作计数改为闭包内真实累加、`rebuild.rs` 模块注释与实现对齐、`source.rs` 32 位长度转换与 mmap 兜底口径修正、`HnswFactory` 自 `index/mod.rs` 拆至 `index/factory.rs`(mod.rs 只留模块组织与 re-export);⑥ 文档同步(01 §模块清单、05 模块清单与 §8 档③条件、03 §8 trait 片段) |
| 2026-09 | 契约漂移修复(增量审计):① 新增 `FC-INDEX-ERR-003`:`hidx::encode` 节点数/邻接区超 `u32` → `LimitExceeded`、单层度数超 `u16` → `Inconsistent`,拒绝静默截断(度数分支定向测试;长度分支为 64 位平台不可达的长度防御,解析登记);② 新增 `FC-PERSIST-ERR-009`:`build_remap` 槽位越界/重复/存在未被版本行引用的段内槽位 → `Corrupted`,绝不静默映射到槽位 0;③ `FC-SCORE-POST-002`(`Scoring::floor`)转 Passed 并补边界测试(等于 floor 保留、低于清零);④ `FC-LIFE-INV-011` 转 Passed(备份目录独立 `open` + `check` 双验);⑤ `FC-CORE-CPLX-001..004/006` 以操作计数单测/解析证明 + 哨兵转 Passed;⑥ `FC-LIFE-CPLX-005`(snapshot/backup/check)转 Passed;⑦ `FC-LIFE-INV-008/017` 注明已落地子项与所属层,整体仍保持 Planned(未落部分属 L5 compaction) |
| 2026-09 | L3 三轮对抗性复审补强:① 档位判断单源化(`GraphTier`)并逐值钉死 `ef` 放大(测试专属探针),杜绝"探针与真实分派漂移";② `reopen` 重排映射改为小 `ef` 多查询召回 ≥0.95 + `ef→∞` 精确双重验收(此前仅大 `ef`,对映射写反/恒等零证伪力);③ `as_of` + ANN 改为注入时钟的真实历史视图(已删记录在历史时点可见、当前不可见),并新增 `snapshot_at` 保留索引句柄单测;④ hidx 构建路径补"入口 = 最高层"断言;⑤ `hidx::encode` 参数收敛为 `GraphParams`(≤4);⑥ 修正档②口径(`ef' = max(ef,k)·4`、`ef'·s ≳ 4k`)并同步 05/14;⑦ 补 `unsafe` 两处口径(`docs/rust/09`、`CONTRIBUTING.md`);`SegmentStat` 加 `#[non_exhaustive]` 并在 16 登记 API 变更 |
| 2026-09 | L3 二轮独立审查整改:① `FC-INDEX-PRE-001` 补全边界并新增 `ef_search ∈ [1, Limits.ef_max]` 校验(此前可绕过查询上限);② 补 `FC-INDEX-POST-001` 档②(放大后过滤)与档③两个独立触发条件的 1:1 测试,档③改为多候选集合相等;③ `FC-INDEX-POST-009` 改为默认 `HnswParams`、两种分布(随机均匀 + 8 簇高斯)微缩数据,并显式设置 `Tuning.brute_force_max_rows` 防静默退化;④ hidx 载入路径强制图不变量(`FC-INDEX-INV-007`:逐层度数 ≤ M0/M、入口层级 = 最高层、`ef_construction ≥ 1`),`FC-INDEX-ERR-001` 增补任意字节 proptest;⑤ 新增 `FC-INDEX-POST-008` 重排映射非恒等(删除后重开)与 `FC-INDEX-POST-005` `as_of` + ANN 验收;⑥ `serialize`/`hidx::encode` 长度转换改为可失败(拒绝静默截断),`SegmentStat` 标 `#[non_exhaustive]`;⑦ 文档同步(放大后过滤口径、unsafe 两处、mmap 读路径口径、03/05/07/14/15/16) |
| 2026-09 | L3 合并前独立审查整改:① 过滤档③补齐「候选数 `< max(ef,1024)` → 暴力」触发,档②由「约束遍历」改为「全图遍历 + 结果限候选」(避免候选稀疏时图被过滤切断导致近空),同步 05 §8/16 §2;② HNSW 参数与过滤阈值建库期校验(`m≥2`/`m0≥m`/`≤4096`/阈值有限且 `brute≤post`,`FC-INDEX-PRE-001`),hidx 头部与逐节点度数同口径;③ `hidx` 缺失/整文件 CRC 不符:fail-fast 拒开、非 fail-fast 降级暴力且 `db.check()` 报告(`FC-INDEX-ERR-002`);④ `stats().segments[*].index_nodes/index_levels` 改取真实载入索引;⑤ 新增 `FC-INDEX-INV-008`(前缀 ANN + 未落盘尾归并 ≡ 全量暴力)、`FC-PERSIST-POST-007`(mmap/文件源读取等价);⑥ 文档同步(`VectorStore` → `memory::index::{VectorIndex,IndexFactory}`、模块清单、冷启动与 mmap 惰性口径、组合根例外) |
| 2026-09 | 落地 L3 索引层:① 新增 `src/index/`(HNSW 构建/查询、过滤三档、hidx HID1 编解码);② 索引经 L1 抽象 `memory::index::{VectorIndex,IndexFactory}` 在门面注入,`WriterState`/`ReaderView` 随快照携带索引,查询前缀走 ANN、未落盘尾部恒暴力,`TopK` 归并(设计 05 §9;原计划 `merge.rs` 在全量快照单图下退化为 `TopK::merge`,已同步 05 §10);③ `flush` 写 `.hidx` 并登记 `hidx_crc`/`entry_slot`/`entry_level`,`open` 经"段内槽位→全局槽位"重排映射从 hidx 载入索引(损坏时降级暴力,`check()` 报告);④ 新增 feature `mmap`(默认开,`memmap2`)与 `MmapSource`,`criterion` dev-dep 与 `benches/hnsw.rs`;⑤ 新增契约 `FC-INDEX-POST-001/002/005/007/008/009`、`FC-INDEX-INV-007`、`FC-INDEX-ERR-001`、`FC-INDEX-CPLX-001..004`(均 Passed),`BitSet` 上移至 `core`(L0) |
| 2026-09 | L2 合并前独立审查整改:① WAL 撕裂尾部在可写重开时**物理截断**(`FC-PERSIST-INV-005`),修复残尾永久屏蔽后续追加导致的数据丢失;② 首次 flush 崩溃(段已写、MANIFEST 未提交)在有 WAL 时以 WAL 重建并清理孤儿段,不再误判 `Corrupted`(`FC-PERSIST-STA-003`,`FC-PERSIST-ERR-005` 收紧为「无 MANIFEST 且无 WAL」);③ MANIFEST 引用的段文件缺失/为空 → `Corrupted`,绝不静默跳过(`FC-PERSIST-ERR-006`);④ 只读打开绝不建目录/清 trash,库目录不存在 → `Config`(`FC-PERSIST-ERR-003` 扩展);⑤ WAL `Insert`/`DeleteRow` 帧携带 `tx_ms`,崩溃恢复后 `as_of` 历史正确(`FC-PERSIST-POST-006`,`FC-PERSIST-POST-005` 补访问统计断言);⑥ WAL 落盘即提交点,后台 flush 失败不回滚已提交写(`FC-PERSIST-INV-006`);⑦ `BatchCommit` 回放校验计数与 `batch_crc`;⑧ 锁改用 `std::fs::File::try_lock` OS 咨询锁(进程终止由内核自动释放,消除 24h 陈旧锁阻塞与接管 TOCTOU,`LOCK` 文件保留不删);⑨ 删除未接线的 `src/persist/delta/**`(孤儿实现,区域保留给 L5);⑩ 未受信长度按剩余字节数设预分配上界;⑪ 移除 `#[allow(dead_code)]`,删除/测试化真正未用项;`FC-PERSIST-CPLX-007` 修正为 L2 整段读入口径(不再声称 mmap 惰性) |
| 2026-09 | L2 复审补齐(契约漂移修复):① 目录状态不一致(`current` 存在但无合法 MANIFEST;存在段文件却无 MANIFEST)→ `Corrupted`,绝不覆盖(新增 `FC-PERSIST-ERR-005`);② 段主版本过新即使默认非 fail-fast 也拒绝打开(修 I18 此前被降级为跳过,`FC-PERSIST-ERR-002` 收紧);③ `check()` 扩展为逐段 CRC + 版本链校验(设计 16 §1.6);④ 只读打开不创建/改写 WAL、空事务不误报(`FC-PERSIST-ERR-003` 补测试);⑤ MANIFEST 未引用的段孤儿在可写打开时清理(`FC-PERSIST-STA-002` 补测试);⑥ 时钟回拨单调钳制 `MonotonicClock`(设计 04 §10.2,`FC-GLOBAL-PRE-005` 转 Passed);⑦ 16 §1.1 未落地 setter(encryption/compression/storage/read_only_probe_interval)标注所属层 |
| 2026-09 | L2 范围界定(文档对齐,与实现一致):① 公开 `Storage` trait 与 `Builder::storage` 推迟到 **L12**(WASM/OPFS 首个非 `FsStorage` 后端出现时抽取),L2 用 `persist/storage.rs` 的 `std::fs` 自由函数,已同步 12 §3.1 与 16 §1.1;② feature `encrypt`/`compress`/`compress-zstd` 的**实现**归 [11 安全存储](../design/11-security-storage.md),L2 仅按 04 §2.5 预留 `header_len` 扩展区(`key_id`/`codec` 缺省 0),磁盘布局在 feature 关闭时逐字节成立;③ `memmap2`/`MmapSource`(L3)、`VectorStore`(L3) 沿用前条目裁定 |
| 2026-09 | L2 完整性补齐:① `touch`/`relate`/`unrelate` 落 WAL(`WriteOp::Access/Relate/Unrelate`,回放重建访问统计与关系边,`FC-PERSIST-POST-005` 转 Passed);② WAL **组提交**(一个写事务仅 1 次 fsync,`FC-PERSIST-CPLX-001` Passed)与 **WAL 容量自动兜底**(达 `wal_bytes` 触发全量快照 flush,`FC-PERSIST-INV-004` Passed);③ `stats()` 回填真实段/WAL/trash 统计(`FC-PERSIST-INV-003` Passed);④ 恢复清理崩溃残留 `.tmp`(`FC-PERSIST-STA-002` Passed)、已提交段 write-once(`FC-PERSIST-STA-001` Passed)、Checkpoint 先物化后截断(`FC-PERSIST-POST-002` Passed)、`verify_on_open`+fail-fast 损坏拒绝(`FC-PERSIST-INV-002` 补测试);⑤ `wal::visit_frames` 流式回放(空间 $O(1)$)。L2 `FC-PERSIST-CPLX-001..003、006..010` 以「解析证明 + 哨兵/操作计数」置 Passed;`CPLX-004/005`(zone map/bloom 扫描评估)属 L4 查询层,保持 Planned 并注明 L2 仅占位空区 |
| 2026-09 | 落地 L2 加固(M3):新增 `persist/hook.rs` 的公开 `FsyncHook`/`IoAction` 与 `Builder::fsync_hook`(测试崩溃注入,设计 04 §10.1),WAL 写/fsync 与段/MANIFEST 写前置触发;`Store::backup_to` 实现(flush → 复制段/MANIFEST/WAL → `current` 最后写,产物可独立 open,设计 16 §7);`Mneme::backup_to` 接线。新增测试 `injected_wal_failure_keeps_confirmed_prefix`(FC-PERSIST-INV-001)与 `backup_is_independently_openable`(新增 `FC-PERSIST-POST-004`);新增 `FC-PERSIST-POST-005`(Planned:`touch`/`relate` WAL 帧持久性待补,当前仅经 flush 快照持久),`FC-PERSIST-INV-019` 收敛为「记录级」 |
| 2026-09 | 落地 L2 持久化接线(M2):新增 `persist/store.rs` 协调句柄(实现内存引擎 `PersistHook`,WAL-before-visible + 批 `BatchBegin/Commit` + Checkpoint 重置);`flush` 采用 **全量快照**(设计 04 §3.2 的 L2 兜底)写 vsec/msec + write-once MANIFEST,`open` 由「MANIFEST/段/WAL」重建状态并回放 `seqno > watermark`;`close` 先 flush 再释放锁。`FC-PERSIST-INV-001/019/020`、`FC-PERSIST-POST-001/003`、`FC-PERSIST-ERR-001/002/003/004` 置 `Passed`(测试见 `tests/persist_contracts.rs` 与 `src/persist/*.rs` 单测);`FC-MEM-ERR-002` 收紧:`open`/`path` 已落地,新建持久库缺维度改返回 `Config`。WAL `Insert` 负载补 `[u32 dim][f32×dim]` 向量副本(设计 04 §2.3 的记录体不含向量,否则未 flush 记录重启后丢失),已同步 04 §2.3。门面(`memory::Builder`/`Mneme`)作为单一 crate 组合根引用 `persist`(构成根例外,不改变 L0→L6 业务依赖方向) |
| 2026-09 | 文档对齐(实现与设计一致性):① `VectorStore` 内部 trait 按 03 §8/04 §14 明确为 **L3 随 HNSW 引入**,L1/L2 直接实现同一组公开签名,同步修订 01 §2「接口先于实现」表述;② 04 §11 明确 L2 仅提供 `FileSource`,`MmapSource`/`mmap` 自 L3 引入(依 01 §5 依赖表);③ 04 §10.1 `FsyncHook`/`IoAction` 明确经 `Builder::fsync_hook` 公开注入(测试 seam),不再表述为"仅测试 builder 暴露" |
| 2026-09 | 启动 L2 持久层实现:① 新增 `crc32fast` 直接依赖(白名单 L2)、`tempfile` dev-dep;② 按 01 §5 依赖表(§5 权威)`memmap2` 自 L3 引入,L2 仅提供 `SegmentSource`+`FileSource`(std),`MmapSource`/`mmap` feature 推迟至 L3,并据此澄清 04 §11;③ `VectorStore` 内部接口(`Target`/`SearchOpt`/`EntryRef`)按 03 §8/04 §14 随 **L3 HNSW** 引入,L2 **不**引入、持久层直接复用公开签名(后续「文档对齐」「范围界定」条目已确认);④ L2 契约测试文件 `tests/persist_contracts.rs` 登记进 `tests/contract_traceability.rs` 双向追溯门禁,条目自 M1 起由 `待补` 回填真实测试路径 |
| 2026-09 | 终审扫尾整改:① `supersede` 此前不查墓碑,`delete` 后调用会把记录复活;现与 `update` 同口径——已墓碑记录返回 `NotFound`、绝不复活(FC-MODEL-POST-003 收紧)。② `WriterState::commit_version` 新增 key 占用预检:新版本携带的 key 若已被**另一可见记录**占用(合并/替换回调改变 key 时的冲突),返回 `DuplicateKey` 并整体回滚,绝不静默覆盖他人 `key_index`(FC-MEM-POST-007 收紧,§0.2 `DuplicateKey` 触发条件扩展)。③ `touch`/`touch_by_rowid` 此前不查逻辑过期,现仅对可见记录生效、否则返回 `false`,与 `feedback`/读路径同口径(新增 FC-MEM-POST-009)。④ `BitSet` 拆出 `src/memory/bitset.rs`,`table.rs` 收敛至 400 行内(rust 规范 §3) |
| 2026-09 | 合并前阻断项修复(B1):① `supersede` 新版本此前直接采用 `rec.key`,省略 key 时丢失目标 key 并留下悬挂 `key_index`(违反新增的 FC-MEM-POST-008,使 `get`/`check`/带 key 的 `as_of` 失准)。现改为信念修订沿用目标 key(省略即继承,显式冲突 → 新增 `KeyMismatch`),并在 `WriterState::link_version` 中于 key 变化时移除旧映射(覆盖 `Dedup::Merge` 改 key 场景),同步 §0.2 错误矩阵与 FC-MODEL-POST-003/FC-MEM-POST-007。② 引入写事务 `Table::write_tx`:所有写操作失败回滚到操作前状态且不发布,修复单条 `insert`/`feedback` 等失败后残留半写(如命名空间登记、访问计数、关系边)并在下次 publish 泄漏的问题,落实 FC-MEM-POST-002 零部分写入(新增 `failed_writes_do_not_register_namespace`)。③ 补 FC-MEM-POST-008 的逻辑过期用例(`check_reports_healthy_after_expiry`)。④ Rust 规范整改:`validate_insert`/`validate_patch` 拆分共享校验、`find_duplicate`/`rerank_composite` 收敛为参数结构体、`cluster_by_similarity`/`iter_with` 降低嵌套、`engine`/`namespace::query` 拆分至 400 行内 |
| 2026-09 | 合并前终审整改:① `check()` 误报修复——此前把「key 索引指向墓碑行」当作不一致,导致任意 `delete`/`drop_namespace` 后健康库 `ok=false`;现改为按最新版本 `ns_id`/`key` 对账,墓碑/逻辑过期不算不一致(新增 FC-MEM-POST-008);② `InsertMode::RejectDuplicate` 逻辑过期记录视为不存在(此前过期记录仍 `DuplicateKey`,与 `exists`/`Dedup::Reject` 口径不一致),`FC-MEM-POST-001` 措辞收紧;③ `WriterState::commit_version` 先校验槽位容量再遮蔽旧版本,消除容量溢出时「已遮蔽但无新版本」的半写,`supersede` 的旧版本 `valid_to` 闭合移至提交成功之后,落实 FC-MEM-PRE-002 零部分写入;④ `feedback` 幂等键改为生效后再登记,失败不占用;`feedback_seen` 经 `Arc` COW,批量快照/回滚不再深拷贝(修正 `WriterState` 注释) |
| 2026-09 | 终审修复:消除两处「非有限策略参数静默失效」——MMR `Diversity::Mmr { lambda }` 含 `NaN` 时 `clamp` 不生效、排序静默退化,现于 `execute()` 入口返回 `Config`(FC-MEM-PRE-003/FC-GLOBAL-PRE-004);`Retention::access_weight` 含 `NaN` 时 `retain` 恒不遗忘,现与 `min_importance` 一并校验为有限值 → `Config`(FC-LIFE-POST-002/FC-GLOBAL-PRE-004)。`insert_batch` 在预校验后仍可能因 `Dedup::Merge` 回调产物超限或槽位溢出中途失败,现以写状态快照回滚,保证整批零部分写入(FC-MEM-POST-002)。内部辅助路径(dedup 判重、`stats` 计数、`consolidate` 候选、`forget` 目标)与常规读路径对齐,一律排除逻辑过期记录(FC-LIFE-INV-009)。修正 `limits.rs` 残留 `Invalid` 注释与 `iter`/`SnapshotNamespace::iter` 残留「流式」措辞(rust 规范 §4.5) |
| 2026-09 | 合并前审查修复:补齐 4 处「契约声称 > 测试证明」缺口(关系边权 NaN→`NonFinite` 与越界钳制、`get_many_by_rowid` 顺序保持、supersede/merge 超限同口径、综合打分因子/MMR lambda 钳制),FC-MEM-PRE-003 的「`Scoring` 越界钳制」措辞修正为与实现一致的「综合打分各因子与 MMR lambda 钳制到 `[0,1]`」(权重本身无 `[0,1]` 定义域,各因子经内部钳制保证落在单位区间);`FC-CORE-CPLX-005` 测试指针改为真实 varint 测试;`FC-SCORE-INV-027` 补充 `QueryId` 进程级分配口径;追溯门禁增强(路径级引用校验、`Passed` 必须登记真实测试、测试文件 FC 声明 ↔ 契约覆盖双向相等校验);`FC-CORE-INV-001` 等价性测试容差按 f32 累加舍入误差模型(γ_n·Σ|aᵢbᵢ|)随长度缩放,修复长向量下的偶发误报(契约措辞「容差内」不变);README/AGENTS 实现状态与文档悬空引用同步 |
| 2026-09 | 合并前契约审计复审修复:`feedback` 对不可见记录的判断补齐「逻辑过期」维度(此前仅查墓碑,过期记录会被误强化并占用幂等键,FC-SCORE-INV-027 实现落地);`dedup_threshold`/`threshold` 校验收紧为「`[0,1]` 内的有限值」(FC-GLOBAL-PRE-004/FC-MODEL-POST-006,与 `Builder::dedup_threshold` rustdoc 已声明的定义域对齐);并行扫描候选收集处的槽位下标 `expect` 登记为 `FC-GLOBAL-ERR-001` 第三处文档化例外(`FC-MEM-INV-004` 保证不可达);iter 的 rustdoc 与 16-api 残留「流式」措辞同步为 CPLX-005 非流式口径 |
| 2026-09 | 合并前契约审计整改(P1):`update`/`update_by_rowid` 补丁路径补齐 text/metadata/provenance 限额校验(与 `insert` 同口径,FC-MEM-PRE-002/FC-GLOBAL-PRE-003);`SearchBuilder::fusion` 单独设置即返回 `Unsupported{feature}`(FC-MEM-ERR-002 落地,消除静默忽略);`importance`/`confidence`/边权/`touch` boost 含非有限值(NaN)→ `NonFinite` 拒绝、绝不入库(FC-GLOBAL-PRE-004 扩展,§0.2 `NonFinite` 语义扩至标量因子);`consolidate` 策略参数非法(`threshold` 非有限值、`max_cluster = 0`)与 `dedup_threshold`/`min_importance` 非有限值 → `Config`(此前为静默空转,`max_cluster = 0` 更会索引越界 panic,违反 FC-GLOBAL-ERR-001);并行扫描工作线程异常终止 → `Inconsistent`(绝不静默少结果);`feedback` 对不可见记录返回 `false` 且不占用幂等键(FC-SCORE-INV-027 语义收紧);CPLX-005 空间口径修正为 $O(N_c)$(物化 `Arc` 句柄,不复制记录体) |
| 2026-09 | Rust 规范审计整改:补齐 L1 门面方法、`pred` 组合器与 varint 解码的 rustdoc `# Arguments` 段(约 55 处,纯文档,不改语义,并消除此前"L0 已补全"记录与 varint 实际状态的偏差);`SnapshotNamespace` 补 `Debug` 实现(加法性变更);`apply_patch` 拆分超 50 行函数;`search_builder.rs` 按主题拆出内部模块 `search_exec.rs`(公开 API 与执行语义不变);`FC-CORE-ERR-001` 测试断言从 `is_err()` 强化为 `Corrupted` 变体匹配 |
| 2026-09 | 规范审计整改:§9.2.8 新增 FC-MODEL-CPLX-002(`consolidate` 聚类两两余弦 $O(n^2\cdot d)$,此前为未声明复杂度路径);公开 API 命名与签名按 rust 规范收敛(`relate_with_meta` → `relate_with_options` + `RelateOptions` 参数结构体,`Retention::w` → `access_weight`;布尔开关经项目所有者确认保留动词短语链式命名,按规范 §14 列为已声明偏差),同步 16-api-reference.md,语义不变 |
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
| 2026-09 | 新增 L1 内存引擎契约(FC-MEM-*):写入校验/批量原子/更新可见/墓碑/点读/去重/并发快照;`RecordRef` 由「借用字段」改为「`Arc` 持有 + 访问器方法」(见 03 §2.2、16 §1.2),并登记 L1 阶段返回结构化 `Invalid` 的延后能力(BM25/DSL/持久化) |
| 2026-09 | FSVDD 审计整改:新增 §0.2 错误分类矩阵并**移除 `Invalid(&'static str)`**,拆为 `Closed`/`NonFinite`/`LimitExceeded`/`MetaTooDeep`/`Config`/`Unsupported`/`Inconsistent`;补 FC-MEM-PRE-004(检索维度)、FC-MEM-INV-004(SlotId 不溢出)、FC-MEM-STA-001(库生命周期)、FC-QUERY-POST-001(谓词类型)、FC-MODEL-POST-006(聚类阈值)、FC-LIFE-POST-002(遗忘分公式);`FC-MODEL-STA-001` 改写为五元组并补测试;CPLX 验证方式显式化(CPLX-001 操作计数,其余解析证明+哨兵);测试文件 `l1_contracts.rs` 更名 `memory_contracts.rs` 对齐 `core_contracts.rs`;追溯门禁由 `xtask check-contracts` 改为 `tests/contract_traceability.rs` |

### 0.2 错误分类矩阵(Error Taxonomy)

> `MnemeError`(`src/core/error.rs`,标注 `#[non_exhaustive]`)是整库唯一对外错误类型。
> **每个偏离形式化约束的语义类都必须有专属变体**——禁止用一个泛化的 `Invalid(&'static str)`
> 承载多种互不相同的失败(FSVDD §2.5)。下表是错误语义的唯一真实数据源。

| 变体 | 触发条件 | 对应 FC | 备注 |
|---|---|---|---|
| `Io` | 底层 I/O 失败 | — | `#[from] std::io::Error` |
| `Corrupted { segment, reason }` | CRC/魔数不符等数据损坏 | FC-CORE-ERR-001、FC-INDEX-ERR-001、FC-INDEX-ERR-002 | `segment=None` 表示文件级损坏 |
| `DimensionMismatch { expected, got }` | 向量长度 ≠ 建库维度(写/查) | FC-GLOBAL-PRE-001、FC-MEM-PRE-001/004 | |
| `MetricMismatch { existing, requested }` | 打开时度量与库中记录不符 | — | 保留,L2 起使用 |
| `KeyMismatch { expected, got }` | `supersede` 的新记录自带 key 与目标 key 冲突(信念修订须沿用同一 key) | FC-MODEL-POST-003 | 新记录省略 key 时继承目标 key |
| `KeyNotFound(Key)` | 键不存在 | — | 保留,当前无 API 产生 |
| `DuplicateKey(Key)` | `InsertMode::RejectDuplicate` 命中,或写版本携带的 key 已被另一可见记录占用(key 迁移冲突) | FC-MEM-POST-001、FC-MEM-POST-007 | |
| `FilterParse(String)` | 过滤 DSL 语法错误(带位置) | FC-QUERY-ERR-001 | L4 起使用 |
| `Busy(&'static str)` | 独占锁被占/备份中 | — | 保留 |
| `TooLarge { field, limit, got }` | 字段载荷超限额(key/text/meta 字节) | FC-GLOBAL-PRE-003、FC-MEM-PRE-002 | |
| `UnsupportedVersion { file, found, max }` | 文件主版本过新 | FC-PERSIST-ERR-002、FC-INDEX-ERR-001 | L2 起使用 |
| `Closed` | 库已关闭后经任意句柄读写 | FC-MEM-ERR-001、FC-MEM-STA-001 | 原 `Invalid("closed")` |
| `NonFinite` | 向量分量或 `importance`/`confidence`/边权/`boost` 等数值输入含 `NaN`/`±Inf`(会污染打分、遗忘公式与排序) | FC-GLOBAL-PRE-002、FC-GLOBAL-PRE-004、FC-MEM-PRE-001/003 | 原 `Invalid("向量分量必须是有限值")` |
| `LimitExceeded { field, limit, got }` | 参数越上限(维度、`top_k`、`ef`、`ef_search`、HNSW 度数) | FC-CORE-PRE-001、FC-GLOBAL-PRE-004、FC-MEM-PRE-003、FC-INDEX-PRE-001 | 原 `Invalid("top_k 超过上限")` 等 |
| `MetaTooDeep { limit, got }` | metadata 嵌套深度超限 | FC-GLOBAL-PRE-003、FC-MEM-PRE-002 | 原 `Invalid("metadata 嵌套过深")` |
| `Config { reason }` | 建库/查询配置非法(缺维度、无查询通道、MMR `lambda` 非有限值)、策略参数含非有限值(`min_importance`/`access_weight`/`threshold`/`dedup_threshold`)或越界(`dedup_threshold`/`threshold` ∉ [0,1])或非法(`max_cluster = 0`)、HNSW 参数域非法(`m < 2`/`m0 < m`/`ef_construction = 0`/`ef_search = 0`/过滤阈值越界或 `brute > post`) | FC-MEM-STA-001、FC-MEM-PRE-003、FC-MODEL-POST-006、FC-LIFE-POST-002、FC-GLOBAL-PRE-004、FC-INDEX-PRE-001 | 原 `Invalid("新建内存库必须指定维度")` 等 |
| `Unsupported { feature }` | 能力延后到后续层,绝不静默降级(`Fusion` 单独设置即拒绝,无需 text 通道;`open`/`path`/持久库 `backup_to` 已在 L2 落地;只读模式写亦返回本变体) | FC-MEM-ERR-002、FC-PERSIST-ERR-003 | BM25/Fusion/text、只读写、**纯内存库** backup |
| `Inconsistent { reason }` | 内部不变量被破坏 | FC-MEM-INV-004 | 原 `Invalid("去重命中但记录不可见")` |

> **迁移说明(破坏性变更)**:`Invalid(&'static str)` 已移除。调用方若曾按字符串匹配
> `Invalid("closed")`,须改用 `Closed`;其余按上表替换。`MnemeError` 为 `#[non_exhaustive]`,
> 新增变体不破坏下游通配匹配,但移除变体属破坏性变更,已在设计 16 §4 记录迁移口径。

---

## 1. 原语层(core / L0)

> 无 I/O、无全局状态、无锁的纯类型与纯函数;`unsafe` 仅在 `simd.rs` 的 arch 内联
> 与 L2 `persist/source.rs` 的 `MmapSource` 两处(均附 `// SAFETY:`)。
> 层边界契约见 [02 §9](../design/02-l0-core.md)。

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-CORE-PRE-001 | PRE | `Dimension::new(d)` 仅接受 `d ∈ [1, 65536]`;越界返回 `LimitExceeded`,内部不再使用裸整数 | `tests/core_contracts.rs::dimension_bounds` | Passed |
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
| FC-MEM-INV-004 | INV | `SlotId` 随物理版本单调递增、永不复用;槽位容量溢出(`u32::MAX`)返回结构化错误,绝不静默饱和 | `src/memory/table/state.rs::slot_id_for_rejects_overflow` | Passed |
| FC-MEM-STA-001 | STA | 库生命周期五元组 `M=(States={Open,Closed}, Events={Close}, δ(Open,Close)=Closed, δ(Closed,Close)=Closed, s0=Open, F={Closed})`;`Closed` 后经任意句柄读写 → `Closed`;重复 `close` 幂等返回 `Ok` | `tests/life_contracts.rs::database_lifecycle_open_closed` | Passed |
| FC-MEM-ERR-001 | ERR | `close` 后(经任意克隆句柄)读写返回 `Closed`;重复 `close` 返回 `Ok`(幂等) | `tests/life_contracts.rs::closed_database_rejects_operations` | Passed |
| FC-MEM-ERR-002 | ERR | 尚未落地的能力以结构化 `Unsupported{feature}` 返回、绝不静默:`text`/`Fusion`(L4,`Fusion` 单独设置即拒绝)、**纯内存库** `backup_to`(持久库已在 L2 实现,见 `FC-PERSIST-POST-004`);`open`/`path` 已在 L2 落地(新建持久库缺维度返回 `Config`,不再是 `Unsupported`) | `tests/life_contracts.rs::deferred_features_return_structured_errors` | Passed |

---

## 2. 持久层(persist)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-PERSIST-INV-001 | INV | **I1**:已确认写入不半写;未确认写入重启后要么完整可见要么不存在;写事务 append/sync 失败时截断半写帧(失败写绝不持久、不遮挡后续已确认写) | `tests/persist_contracts.rs::reopen_after_close_recovers_records`、`tests/persist_contracts.rs::reopen_after_drop_recovers_from_wal`、`tests/persist_contracts.rs::injected_wal_failure_keeps_confirmed_prefix`、`tests/persist_contracts.rs::injected_fsync_failure_rolls_back_frames` | Passed |
| FC-PERSIST-INV-002 | INV | **I2**:任意 bit 损坏可检出或拒绝启动,绝不静默返回错误数据 | `src/persist/vsec.rs::vsec_detects_header_corruption`、`src/persist/vsec.rs::vsec_detects_payload_corruption`、`src/persist/manifest.rs::manifest_detects_header_corruption`、`src/persist/manifest.rs::manifest_detects_payload_corruption`、`src/persist/wal/mod.rs::wal_bad_crc_stops_replay`、`tests/persist_contracts.rs::verify_on_open_detects_payload_corruption`、`tests/persist_contracts.rs::check_detects_corrupt_segment` | Passed |
| FC-PERSIST-INV-003 | INV | **I3**:活跃段集合 = 某 MANIFEST 版本所列集合 | `tests/persist_contracts.rs::flush_checkpoints_after_materialize` | Passed |
| FC-PERSIST-INV-004 | INV | **I4**:WAL 总量 ≤ `wal_bytes`;段文件只增不改 | `tests/persist_contracts.rs::wal_capacity_triggers_snapshot_flush`、`tests/persist_contracts.rs::committed_segment_is_write_once` | Passed |
| FC-PERSIST-INV-005 | INV | **I1(撕裂尾部)**:可写重开时物理截断 WAL 撕裂尾部(有效长度 = 头部 + 完整帧),使截断之后的追加写入不被残尾永久屏蔽;有效前缀不受影响;WAL 短于文件头(Checkpoint 重置中途崩溃)视为撕裂头并重建,不拒绝打开 | `tests/persist_contracts.rs::torn_wal_tail_truncated_on_reopen`、`tests/persist_contracts.rs::short_wal_header_is_recreated_on_open` | Passed |
| FC-PERSIST-INV-006 | INV | **I1(提交点)**:WAL 帧成功 fsync 即为提交点;其后后台 flush 失败不回滚已提交写,绝不出现「返回 `Err` 但重启后可见」的矛盾(flush 失败由下次写重试,`stats().wal_bytes` 可观测) | `tests/persist_contracts.rs::flush_failure_does_not_lose_committed_write` | Passed |
| FC-PERSIST-INV-019 | INV | **I19(记录级)**:`delete`/`update` 返回 `Ok` 后,崩溃 + WAL 截断仍生效,删除永不复活(`touch`/`relate` 的 WAL 帧见 FC-PERSIST-POST-005) | `tests/persist_contracts.rs::delete_survives_flush_and_reopen`、`tests/persist_contracts.rs::crash_after_delete_does_not_resurrect`、`tests/persist_contracts.rs::update_survives_reopen` | Passed |
| FC-PERSIST-INV-020 | INV | **I20**:`path↔NsId`、`next_ns_id`、`next_rowid` 可由 MANIFEST+WAL 重建,ID 永不复用 | `tests/persist_contracts.rs::namespace_and_rowid_survive_reopen` | Passed |
| FC-PERSIST-POST-001 | POST | **I15**:`insert_batch` 整批原子:可见记录数 ∈ {0, n},无部分批 | `tests/persist_contracts.rs::batch_insert_is_atomic_across_reopen` | Passed |
| FC-PERSIST-POST-002 | POST | Checkpoint 仅当 `seqno ≤ watermark` 的覆盖条目已物化时才截断 WAL | `tests/persist_contracts.rs::flush_checkpoints_after_materialize` | Passed |
| FC-PERSIST-POST-003 | POST | **I16**:`close()` 返回 `Ok` 后所有已确认写入持久;`Drop` 不保证 | `tests/persist_contracts.rs::reopen_after_close_recovers_records` | Passed |
| FC-PERSIST-POST-004 | POST | `backup_to` 先 flush 再复制段(vsec/msec/hidx)/MANIFEST/WAL,`current` 最后写;产物可独立 `open`(设计 16 §7) | `tests/persist_contracts.rs::backup_is_independently_openable` | Passed |
| FC-PERSIST-POST-005 | POST | `touch`/`relate`/`unrelate` 的 WAL 帧持久性:崩溃后回放 `TouchRow`/`Relate`/`Unrelate` 帧,`access_count`/访问时刻与关系边不丢失(importance 强化随版本 `Insert`)、unrelate 不复活 | `tests/persist_contracts.rs::relate_and_unrelate_survive_crash`、`tests/persist_contracts.rs::touch_boost_survives_crash`、`src/persist/wal/mod.rs::wal_touch_relate_roundtrip` | Passed |
| FC-PERSIST-POST-006 | POST | WAL `Insert`/`DeleteRow` 帧携带版本事务时间 `tx_ms`;崩溃恢复后 `as_of(t)` 历史正确(删除前时点可见、删除后不可见),不以记录体 `created_at` 或 `0` 代替 | `tests/persist_contracts.rs::as_of_history_survives_reopen` | Passed |
| FC-PERSIST-ERR-001 | ERR | 未知 WAL 帧类型 → 停止回放并报错,不静默跳过 | `src/persist/wal/mod.rs::wal_unknown_frame_type_errors` | Passed |
| FC-PERSIST-ERR-002 | ERR | 更高主版本 → `UnsupportedVersion`(I18),段级过新版本即使默认非 fail-fast 也拒绝打开、绝不降级为跳过 | `src/persist/vsec.rs::vsec_rejects_higher_major`、`src/persist/manifest.rs::manifest_rejects_higher_major`、`tests/persist_contracts.rs::higher_major_segment_is_rejected` | Passed |
| FC-PERSIST-ERR-003 | ERR | 只读模式写操作 → `Unsupported { feature: "只读模式写入" }`,绝不静默;只读打开不创建/改写 WAL(设计 04 §13);只读打开绝不改动文件系统(不建目录、不清 `trash/`),库目录不存在 → `Config` | `tests/persist_contracts.rs::read_only_rejects_writes`、`tests/persist_contracts.rs::read_only_open_does_not_create_wal`、`tests/persist_contracts.rs::read_only_open_does_not_mutate` | Passed |
| FC-PERSIST-ERR-004 | ERR | 打开时显式维度与 MANIFEST 不符 → `DimensionMismatch`,拒绝打开(设计 16 §3) | `tests/persist_contracts.rs::dimension_mismatch_rejected_on_open` | Passed |
| FC-PERSIST-ERR-005 | ERR | 目录状态不一致(`current` 存在但无合法 MANIFEST,或存在段文件却既无 MANIFEST 也无 WAL)→ `Corrupted`,绝不当作新库覆盖既有数据(设计 16 §3)。注:段文件存在但**有 WAL** 属首次 flush 崩溃,见 `FC-PERSIST-STA-003`,不返回 `Corrupted` | `tests/persist_contracts.rs::corrupt_current_without_valid_manifest_is_rejected`、`tests/persist_contracts.rs::segments_without_manifest_are_rejected` | Passed |
| FC-PERSIST-ERR-006 | ERR | MANIFEST 引用的段文件缺失或为空 → `Corrupted`,绝不静默跳过而少返回数据(I2/I3) | `tests/persist_contracts.rs::referenced_segment_missing_is_rejected`、`tests/persist_contracts.rs::referenced_segment_empty_is_rejected` | Passed |
| FC-PERSIST-ERR-007 | ERR | WAL `BatchCommit` 的批内帧计数与 `batch_crc` 在回放时校验;不符 → `Corrupted`,拒绝应用半批,绝不静默(I15);未闭合批(缺 `BatchCommit`)不计入已提交长度,重开时截断,绝不吞掉其后单操作事务 | `src/persist/recover/replay.rs::replay_rejects_mismatched_batch_crc`、`src/persist/recover/replay.rs::replay_rejects_mismatched_batch_count`、`src/persist/recover/replay.rs::replay_applies_well_formed_batch`、`src/persist/recover/replay.rs::unclosed_batch_is_not_committed`、`tests/persist_contracts.rs::unclosed_batch_tail_does_not_swallow_later_writes` | Passed |
| FC-PERSIST-ERR-008 | ERR | 独占锁基于 OS 咨询锁(`std::fs::File::try_lock`):活实例持有 → `Busy`;进程崩溃/退出时内核自动释放,后续实例无需租约/接管即可获取;`Drop` 释放锁但不删除锁文件,避免不同 inode 各自加锁破坏互斥(设计 16 §3) | `src/persist/storage.rs::file_lock_blocks_second_holder`、`src/persist/storage.rs::file_lock_acquires_when_lock_file_exists`、`src/persist/storage.rs::file_lock_file_persists_after_drop` | Passed |
| FC-PERSIST-ERR-009 | ERR | 单段带 `hidx` 恢复时的槽位重排映射(`recover::state::build_remap`):版本行槽位越界、重复,或存在未被任何版本行引用的段内槽位(vsec/msec 行数不一致)→ `Corrupted`,绝不静默把未引用槽位映射到槽位 0;无 `hidx`/多段/存在跳过段时返回 `None`(调用方降级暴力,属设计行为而非错误);载入期二次校验:重排映射指向不存在槽位 → `Corrupted` | `src/persist/recover/state.rs::build_remap_maps_slots_in_version_chain_order`、`src/persist/recover/state.rs::build_remap_rejects_out_of_range_duplicate_or_unreferenced_slots`、`src/persist/store/open.rs::load_index_rejects_remap_past_state_slots` | Passed |
| FC-PERSIST-STA-001 | STA | 段生命周期:`Building → Committed → Obsolete → (trash)`;`Committed` 段内容不可变(write-once,重写产生新段) | `tests/persist_contracts.rs::committed_segment_is_write_once` | Passed |
| FC-PERSIST-STA-002 | STA | 崩溃点状态:`Building` 段(`.tmp` 半成品)与 MANIFEST 未引用的段为孤儿,可写打开时清理;不进入任何 MANIFEST 视图 | `tests/persist_contracts.rs::orphan_tmp_cleaned_on_open`、`tests/persist_contracts.rs::unreferenced_segment_cleaned_on_open` | Passed |
| FC-PERSIST-STA-003 | STA | 首次 flush 中途崩溃(段已写、MANIFEST 未提交):存在 WAL 时以 WAL 为准重建,孤儿段被清理,绝不误判为 `Corrupted` 而丢数据 | `tests/persist_contracts.rs::first_flush_crash_recovers_from_wal` | Passed |
| FC-PERSIST-POST-007 | POST | 段读取后端等价:feature `mmap` 开/关时 `source::read_whole` 与 `std::fs::read` 逐字节一致;`MmapSource::slice` 返回整段、`read_at` 越界 → `UnexpectedEof`(mmap 为优化,不改变功能语义;32 位平台长度转换失败返回 `Io`,不静默截断,解析证明登记) | `src/persist/source.rs::read_whole_matches_bytes`、`src/persist/source.rs::mmap_source_slice_and_bounds` | Passed |

---

## 3. 索引与检索(index/query/score)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-INDEX-INV-005 | INV | **I5**:同一快照内 `execute()` = 候选集内暴力 + 标准融合(统计等价) | 待补 | Planned |
| FC-INDEX-INV-006 | INV | **I6**:过滤先行;结果与融合顺序无关 | 待补 | Planned |
| FC-INDEX-INV-021 | INV | **I21**:BM25 统计按查询命名空间跨全部活跃段全局聚合(df/N/avgdl),只计活行,与段数无关,跨 NS 互不影响 | 待补 | Planned |
| FC-INDEX-PRE-001 | PRE | 建库时校验 HNSW 参数:`m ≥ 2`、`m0 ≥ m`、`ef_construction ≥ 1`、`m`/`m0 ≤ 4096`(硬上限,越限使自产 hidx 无法读回)、`ef_search ≥ 1` 且 `ef_search ≤ Limits.ef_max`(防止默认查询宽度绕过查询期上限);过滤三档阈值 `filter_post_threshold`/`filter_brute_threshold` 为 `[0,1]` 内有限值且 `brute ≤ post`。违反 → `Config`/`LimitExceeded`,绝不静默 | `tests/hnsw_contracts.rs::invalid_hnsw_params_and_thresholds_are_rejected` | Passed |
| FC-INDEX-POST-001 | POST | 过滤三档:①后过滤(`s > post`,全图遍历 + `ef' = max(ef,k)·min(8,1/s)`);②全图遍历 + `ef' = max(ef,k)·4` 后过滤(结果限候选,触发条件 `brute < s ≤ post` 且候选数 ≥ `max(ef,1024)`;不用约束遍历以免图被过滤切断);③候选暴力(**仅当存在过滤位图**:选择性 ≤ `brute_threshold` **或**候选数 < `max(ef,1024)`,两个触发条件各自独立成立;无过滤时不入档③(无过滤时候选即 alive、选择性恒为 1;默认 `post < 1` 走档①,合法边界 `post = 1.0` 走档②,二者同为全图遍历、不改变语义));档③恒等于「候选位图内暴力」,档①②与之统计等价(口径:`ef'·s ≳ 4k`,`ef→∞` 精确;设计 05 §8) | `src/index/filtered.rs::tier_selection_matches_selectivity_and_candidate_cap`、`tests/hnsw_contracts.rs::filter_tier_three_matches_candidate_bruteforce`、`tests/hnsw_contracts.rs::filter_brute_trigger_conditions_are_independent`、`tests/hnsw_contracts.rs::filter_post_tier_matches_candidate_bruteforce`、`tests/hnsw_contracts.rs::filter_amplified_tier_matches_candidate_bruteforce` | Passed |
| FC-INDEX-POST-002 | POST | **I5 收敛**:`ef → ∞` 时 HNSW 结果收敛于精确暴力 | `tests/hnsw_contracts.rs::ann_converges_to_bruteforce_with_large_ef` | Passed |
| FC-INDEX-POST-003 | POST | **排序全等性**:同一快照内任意两次 `execute()`(同参数)结果完全一致(同分按 RowId 升序) | `tests/query_contracts.rs::search_order_is_total_and_stable` | Passed |
| FC-INDEX-POST-005 | POST | ANN 结果 ⊆ alive ∩ 过滤位图,**且 alive 按目标命名空间与 TTL 判定**(其他 NS 与逻辑过期记录不得入选);死节点与未被 alive 选中的历史版本只可穿越、不可入选(设计 05 §7/§12);`as_of` 历史视图按历史 alive 位图返回已删记录、当前视图不返回,且 `snapshot_at` 保留索引句柄(不得静默降级暴力) | `tests/hnsw_contracts.rs::ann_excludes_deleted_records`、`tests/hnsw_contracts.rs::ann_respects_namespace_and_ttl_visibility`、`tests/hnsw_contracts.rs::ann_after_as_of_matches_bruteforce`、`src/memory/temporal.rs::snapshot_at_preserves_index_handle`、`src/index/hnsw.rs::search_results_respect_alive_bitmap` | Passed |
| FC-INDEX-POST-007 | POST | hidx(HID1)编解码往返恢复同一图(节点数/层级/邻接/入口/参数);`decode(encode(g))` 与 `g` 一致 | `src/index/hidx.rs::hidx_roundtrip_restores_graph` | Passed |
| FC-INDEX-POST-008 | POST | 持久库 `flush` 写 `hidx` 并在 MANIFEST 登记 `hidx_crc`/`entry_slot`/`entry_level`;重开经重排映射从 hidx 载入索引(`stats().segments[*].index_nodes > 0`)且检索正确;重排映射在非恒等场景(删除/多版本导致段内槽位次序与全局槽位次序不同)亦正确(小 `ef` 多查询召回 + `ef→∞` 精确双重验收) | `tests/hnsw_contracts.rs::reopen_loads_hnsw_from_hidx`、`tests/hnsw_contracts.rs::reopen_after_delete_remaps_slots` | Passed |
| FC-INDEX-POST-009 | POST | ANN Recall@10 ≥ 0.95(`ef=128`,`HnswParams::default()`,段行数超过 `brute_force_max_rows` 时走图);**分派为严格「超过」:行数 ≤ `brute_force_max_rows` 时恒暴力,与索引是否存在无关**;两种固定种子分布(随机均匀与 8 簇合成数据)分别达标 | `tests/hnsw_contracts.rs::ann_recall_at_ten_meets_threshold`、`src/memory/search.rs::search_dispatches_to_index_when_prefix_exceeds_brute_threshold`、`src/memory/search.rs::search_bruteforces_when_prefix_does_not_exceed_threshold` | Passed |
| FC-INDEX-INV-007 | INV | 图节点 id ∈ [0,count);每层度数 ≤ M0(第 0 层)/ M(上层);无自环、邻居 id 有效;入口节点层级 = 全图最高层。构建与 hidx 载入两条路径恒成立(载入解码即校验,违反 → `Corrupted`) | `src/index/hnsw.rs::graph_degree_and_self_loop_invariants`、`src/index/hidx.rs::hidx_rejects_degree_above_layer_bound`、`src/index/hidx.rs::hidx_rejects_entry_level_below_max` | Passed |
| FC-INDEX-INV-008 | INV | 查询 = 索引前缀 ANN + 未落盘尾部暴力,`TopK` 归并;`ef→∞` 时前缀精确,合并结果 ≡ 全量候选暴力(设计 05 §9 单段退化形态) | `tests/hnsw_contracts.rs::ann_merges_prefix_with_unflushed_tail` | Passed |
| FC-INDEX-ERR-001 | ERR | hidx 魔数不符/负载 CRC 翻转 → `Corrupted`;主版本过新 → `UnsupportedVersion`(I18);头部 `ef_construction = 0`/入口层级低于最高层/逐层度数越界/截断/头 CRC/布局不符 → `Corrupted`;**载入节点数与恢复槽位数不一致 → `Corrupted`**;任意输入不 panic、不静默(proptest「接受即往返」+ 定向用例) | `src/index/hidx.rs::hidx_rejects_bad_magic`、`src/index/hidx.rs::hidx_detects_payload_corruption`、`src/index/hidx.rs::hidx_rejects_higher_major`、`src/index/hidx.rs::hidx_rejects_zero_ef_construction`、`src/index/hidx.rs::hidx_rejects_entry_level_below_max`、`src/index/hidx.rs::hidx_rejects_degree_above_layer_bound`、`src/index/hidx.rs::hidx_rejects_truncated_or_malformed_header`、`src/index/hidx.rs::hidx_rejects_bad_layout`、`src/index/hidx.rs::hidx_rejects_bad_neighbors`、`src/index/hidx.rs::hidx_decode_never_panics_on_arbitrary_bytes`、`src/index/hnsw.rs::load_rejects_node_count_mismatch` | Passed |
| FC-INDEX-ERR-002 | ERR | MANIFEST 引用的 `hidx` 缺失/整文件 CRC 不符:fail-fast 打开 → `Corrupted`;可写非 fail-fast 打开 → 降级暴力(`stats().segments[*].index_nodes == 0`)、库仍可读,`db.check()` 报告该段损坏 | `tests/persist_contracts.rs::missing_hidx_degrades_or_rejects`、`tests/persist_contracts.rs::corrupt_hidx_degrades_or_rejects` | Passed |
| FC-INDEX-ERR-003 | ERR | `hidx::encode` 编码期防御:图节点数/节点表/邻接区字节数超 `u32` → `LimitExceeded`(`field` 标明具体字段);单层度数超 `u16` → `Inconsistent`(违反 `FC-INDEX-INV-007` 度数上界)。绝不静默截断(`as u32`/`as u16`)。`Builder` 校验(度数 ≤ 4096)下正常构建不可达;长度分支需 >4 GiB 邻接区,64 位平台以解析证明登记,度数分支以定向测试证伪 | `src/index/hidx.rs::hidx_encode_rejects_degree_above_u16` | Passed |
| FC-SCORE-INV-027 | INV | **I27**:同一 `(rowid, query_id)` 的反馈至多计一次;对不可见记录(不存在/已墓碑/已过期)的反馈返回 `false` 且**不占用幂等键**(后续该 `RowId` 重新可见时首次反馈仍生效);`execute()` 缺省生成的 `QueryId` 由进程级全局分配器分配(跨库实例共享同一编号空间,保证不冲突),调用方显式指定时须自行保证唯一性 | `tests/query_contracts.rs::feedback_is_idempotent_per_query` | Passed |
| FC-SCORE-POST-001 | POST | `Scoring::default()` 与未开启 `score()` 的排序全等 | `tests/query_contracts.rs::default_scoring_matches_similarity_order` | Passed |
| FC-SCORE-POST-002 | POST | `Scoring::floor` 下的候选满足 `ŝ ≥ floor` 或 `S = 0`:归一化相似度 `ŝ < floor` 时综合分清零,`ŝ = floor` 为保留边界(实现用严格小于,等于保留);`floor = 0` 时恒不清零 | `tests/query_contracts.rs::scoring_floor_zeroes_below_threshold` | Passed |
| FC-SCORE-POST-003 | POST | 放大 ef 后综合排序相对召回损失 ≤ 2% | 待补 | Planned |
| FC-QUERY-ERR-001 | ERR | DSL 任意输入不 panic,返回结构化 `FilterParse`(I7) | 待补 | Planned |
| FC-QUERY-ERR-002 | ERR | `Not` 对缺失字段采用三值语义(缺失 → `Not` 亦为 false) | `tests/query_contracts.rs::filter_uses_kleene_three_valued_logic` | Passed |
| FC-QUERY-POST-001 | POST | 谓词类型规则:数值比较 `Int`/`Num` 互通;`Ts` 仅与 `Ts` 比较;`Contains`/`StartsWith`/`EndsWith`/`Glob` 要求字符串或数组;类型不匹配求值为 `Unknown`(不命中) | `tests/query_contracts.rs::predicate_type_rules` | Passed |
| FC-INDEX-POST-004 | POST | 写入期去重:`Dedup::Merge` 就地更新并保留旧 RowId(返回 `Merged(old)`);`Dedup::Replace` 生成新 RowId 并墓碑旧行;`insert_batch` 中 `RejectDuplicate`/`Dedup::Reject` 逐条返回 `Duplicate`,不回滚整批 | `tests/memory_contracts.rs::dedup_reject_replace_and_merge`、`tests/memory_contracts.rs::dedup_replace_with_key_gets_new_rowid` | Passed |

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
| FC-MODEL-POST-004 | POST | **版本链保留**:每个 RowId 的最新版本与 `history_horizon` 内的历史版本被保留;仅超期版本被回收,`as_of` 在窗口内可读 | 待补 | Planned |
| FC-MODEL-POST-005 | POST | `predecessors(to, kinds)` 只返回 `edge.to == to` 且 `edge.kind ∈ kinds`、两端存活的边;`RelationIndex::Outgoing` 与 `Both` 结果一致(反向索引只加速、不改语义) | `tests/model_contracts.rs::predecessors_returns_incoming_edges` | Passed |
| FC-MODEL-POST-006 | POST | `consolidate(policy)` 以 `threshold` 为聚类相似度下界:相似度 ≥ threshold 的近似重复聚为一簇,簇成员 ≥ 2 才合并;`keep_sources=true` 不删来源;策略参数非法(`threshold` 非有限值或越界 [0,1]、`max_cluster = 0`)→ `Config`,绝不静默空转或索引越界 | `tests/model_contracts.rs::consolidate_merges_cluster_and_keeps_sources`、`tests/model_contracts.rs::consolidate_rejects_invalid_policy` | Passed |
| FC-MODEL-STA-001 | STA | 记忆版本五元组 `M=(S={Active,Shadowed,Reclaimed}, E={Update,Upsert,Delete,AsOf,Compact}, δ: Active×{Update,Upsert,Delete}→Shadowed(旧)∧Active(新), Shadowed×AsOf→Shadowed(历史可见), Shadowed×Compact→Reclaimed, s0=Active, F={Reclaimed})`;非法转移:当前读路径(`get`/`search`/`iter`/`count`)命中 `Shadowed` 必须不可见并显式拦截,绝不静默返回 | `tests/model_contracts.rs::model_version_lifecycle_states` | Passed |

---

## 5. 生命周期(life)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-LIFE-INV-008 | INV | **I8**:活跃段数 ≤ `(T−1)·log_r(N/B)+c`;WAL ≤ `wal_bytes`。注:WAL 上界部分已随 L2 落地并验收于 `FC-PERSIST-INV-004`;活跃段数上界依赖 L5 compaction,未落部分保持 Planned | 待补(段数部分) | Planned |
| FC-LIFE-INV-009 | INV | **I9**:逻辑过期/墓碑记录在常规读路径永不返回(仅 `iter_with(..., true)` 审计入口可见);内部辅助路径(dedup 判重、`stats` 计数、`consolidate` 候选、`forget` 目标)同样排除逻辑过期记录;物理回收仅在 compaction 提交后 | `tests/memory_contracts.rs::delete_hides_records_from_reads`、`tests/memory_contracts.rs::logically_expired_hidden_from_internal_paths` | Passed |
| FC-LIFE-INV-010 | INV | **I10**:compaction 崩溃 → 恢复后 = 提交前状态(孤儿段清理) | 待补 | Planned |
| FC-LIFE-INV-011 | INV | **I11**:备份目录独立 `open` + `check` 通过(**FC-PERSIST-POST-004** 已覆盖复制语义) | `tests/persist_contracts.rs::backup_is_independently_openable` | Passed |
| FC-LIFE-INV-017 | INV | **I17**:`SnapshotHandle` 视图一致,后台 compaction 不影响。注:`SnapshotHandle` 视图一致部分已随 L1 落地(`tests/memory_contracts.rs::seqno_and_rowid_stable_prop` 观测水位快照);"后台 compaction 不影响"依赖 L5 compaction,未落部分保持 Planned | 待补(compaction 部分) | Planned |
| FC-LIFE-INV-023 | INV | **I23**:自动遗忘默认关闭;删除可审计(墓碑在 `history_horizon` 内保留,默认永久,经 `iter_with(..., true)` 可见),绝不静默 | `tests/life_contracts.rs::retain_forgets_below_threshold` | Passed |
| FC-LIFE-POST-001 | POST | `retain` 返回 `forgotten` 与 `sampled_ids` 与实际墓碑一致 | `tests/life_contracts.rs::retain_forgets_below_threshold` | Passed |
| FC-LIFE-POST-002 | POST | 保留分公式 `score = importance·2^(−age/T½) + w·ln(1+access_count)`;`age = max(0, now − max(valid_from, last_access))`(`valid_from` 取记录有效时间起,`last_access` 取最近访问;`T½=0` 时衰减项为 0,`age<0` 按 0);`min_importance`/`access_weight` 任一含非有限值 → `Config`(绝不静默永不遗忘) | `src/memory/lifecycle.rs::retention_score_formula`、`tests/life_contracts.rs::error_taxonomy_is_specific` | Passed |

---

## 6. 量化与门面(quant)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-QUANT-INV-012 | INV | **I12**:量化模式 `Hit.score` = f32 精排分 | 待补 | Planned |
| FC-QUANT-INV-013 | INV | **I13**:召回不达标自动回退 f32,`stats()` 可见 | 待补 | Planned |
| FC-QUANT-INV-014 | INV | **I14**:async 与 sync API 等价(共享同一写锁) | 待补 | Planned |
| FC-QUANT-ERR-001 | ERR | 未开 `quant-f16` 时 `VectorFormat::F16` 构造期返回 `Unsupported`,不静默降级 | 待补 | Planned |

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
| FC-GLOBAL-PRE-001 | PRE | 向量维度 ∈ [1, 65536];长度 ≠ 建库维度 → `DimensionMismatch`(写/查同口径) | `tests/memory_contracts.rs::insert_rejects_dimension_and_non_finite`、`tests/query_contracts.rs::search_rejects_dimension_mismatch` | Passed |
| FC-GLOBAL-PRE-002 | PRE | 任一分量 `NaN`/`±Inf` → `NonFinite`,库内不被污染 | `tests/memory_contracts.rs::insert_rejects_dimension_and_non_finite` | Passed |
| FC-GLOBAL-PRE-003 | PRE | key ≤ 1024B、text ≤ 1MiB、meta ≤ 64KiB、深度 ≤ 32 → 否则 `TooLarge`/`MetaTooDeep` | `tests/memory_contracts.rs::write_limits_reject_too_large`、`tests/memory_contracts.rs::write_meta_limits_reject_too_large`、`tests/memory_contracts.rs::write_limits_boundary_three_point`、`tests/memory_contracts.rs::update_enforces_write_limits` | Passed |
| FC-GLOBAL-PRE-004 | PRE | `importance`/`confidence` 越界钳制到 [0,1],含非有限值(NaN)→ `NonFinite` 拒绝(含 `UpdatePatch` 与 `touch` boost、关系边权);`top_k`/`ef` > 4096 → `LimitExceeded`;`dedup_threshold`/`threshold`/`min_importance`/`access_weight` 等策略参数与 MMR `lambda` 含非有限值 → `Config`;`dedup_threshold`/`threshold` 越界 [0,1] → `Config` | `tests/memory_contracts.rs::importance_and_confidence_clamped`、`tests/memory_contracts.rs::importance_confidence_boundary_three_point`、`tests/query_contracts.rs::search_limits_reject_top_k_and_ef`、`tests/life_contracts.rs::error_taxonomy_is_specific`、`tests/model_contracts.rs::relate_weight_rejects_non_finite_and_clamps` | Passed |
| FC-GLOBAL-ERR-001 | ERR | 库绝不 panic;文档化例外共三处:① `filter!` 字面量、② async `spawn_blocking`、③ 并行扫描候选收集处槽位下标的 `u32::try_from(..).expect`(`commit_version` 经 `slot_id_for` 拒绝溢出,`FC-MEM-INV-004` 保证不可达) | `tests/life_contracts.rs::l1_api_smoke_never_panics` | Passed |
| FC-GLOBAL-ERR-002 | ERR | 错误分类矩阵(§0.2)各变体语义互不混淆:`Closed`/`NonFinite`/`LimitExceeded`/`MetaTooDeep`/`Config`/`Unsupported`/`Inconsistent` 各由专属条件触发 | `tests/life_contracts.rs::error_taxonomy_is_specific` | Passed |
| FC-GLOBAL-PRE-005 | PRE | 时钟回拨经单调水位钳制(取历史最大值);记录不会因回拨早消失/复活 | `tests/memory_contracts.rs::clock_rollback_does_not_resurrect_expired_record` | Passed |

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
| FC-CORE-CPLX-001 | CPLX | `simd::dot` / `dot_scalar` / `Metric::score`:时间 $O(d)$;AVX2 指令数 $\approx 3d/8+3$;空间 $O(1)$ | 操作计数单测 `src/core/simd.rs::dot_scalar_per_element_cost_is_linear`(逐元素乘加计数 == `d`)+ 解析证明(02 §3.4/§4.3:单遍 $d$ 次乘加,AVX2 每 8 元素 1 FMA + 2 加载) | Passed |
| FC-CORE-CPLX-002 | CPLX | `Metric::better` / `needs_norm`:时间 $O(1)$、空间 $O(1)$ | 解析证明(02 §3.4:两者为常数分支,不含循环)+ 哨兵 `tests/core_contracts.rs::metric_better_direction`、`tests/core_contracts.rs::metric_needs_norm` | Passed |
| FC-CORE-CPLX-003 | CPLX | `TopK::push`:未满 $O(\log k)$、已满 $O(1)$ 拒绝或 $O(\log k)$ 下沉(最坏 $O(\log k)$);`TopK::new` 预分配 $\le \min(k,1024)$;空间 $O(k)$ | 操作计数单测 `src/core/heap.rs::topk_prealloc_bounded_by_min_k_1024`、`src/core/heap.rs::topk_push_outside_k_costs_constant_or_log_k`(拒绝路径恰 1 次比较)+ 解析证明(02 §5.2/§5.4) | Passed |
| FC-CORE-CPLX-004 | CPLX | `TopK::merge` / `into_sorted_vec`:时间 $O(k\log k)$,**与 $N$ 无关**;空间 $O(k)$ | 操作计数单测 `src/core/heap.rs::topk_merge_and_sort_cost_bounded_by_k`(只操作 $\le k$ 个元素,比较次数与扫描规模 $N$ 无关且以 $k\log k$ 为界)+ 解析证明(02 §5.4) | Passed |
| FC-CORE-CPLX-005 | CPLX | `varint::{encode_u32,encode_u64,decode_u32,decode_u64}`:时间 $O(\lfloor\log_{128}x\rfloor+1)\le 10$ 字节操作;空间 $\le 10$ B | 最小编码/10 字节上界断言 `tests/core_contracts.rs::varint_malformed`、`tests/core_contracts.rs::varint_roundtrip_minimal` | Passed |
| FC-CORE-CPLX-006 | CPLX | `meta::get_path`:时间 $O(p)$($p$ = `.` 分段数,每段平均 $O(1)$ 查找);`as_f64/as_i64/as_bool/as_str/as_ts`: $O(1)$ | 解析证明(02 §7:按 `.` 分段逐级下降,每级一次 `Value::get`;访问器为单次类型匹配)+ 哨兵 `tests/core_contracts.rs::meta_accessors` | Passed |

#### 9.2.2 L1 内存层(mem)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-MEM-CPLX-001 | CPLX | 暴力扫描:时间 $O(N\cdot d)$,过滤后 $O(N_c\cdot d)$;空间 $O(N/8)$ 位图 + $O(k)$ | 操作计数单测 `src/memory/search.rs::scan_touches_each_candidate_once`(打分次数 = 候选数)+ 语义哨兵 `tests/query_contracts.rs::brute_force_matches_reference`、PBT `tests/query_contracts.rs::brute_force_matches_reference_prop` | Passed |
| FC-MEM-CPLX-002 | CPLX | 元数据过滤求值:时间 $O(N\cdot\|\phi\|)$,单行 $O(\|\phi\|)$(短路求值);空间 $O(N/8)$ | 解析证明(设计 03 §5.2)+ 哨兵 `tests/query_contracts.rs::filter_uses_kleene_three_valued_logic` | Passed |
| FC-MEM-CPLX-003 | CPLX | 并行归并:时间 $O(C_{\text{block}}\cdot k\log k)$;空间 $O(C_{\text{block}}\cdot k)$ | 解析证明(设计 03 §4.3)+ 哨兵 `tests/query_contracts.rs::search_order_is_total_and_stable` | Passed |
| FC-MEM-CPLX-004 | CPLX | `delete` / `touch`:时间 $O(\log n)$ 定位 + $O(1)$ 墓碑/统计更新;空间 $O(1)$ | 解析证明(HashMap 定位 + 版本链追加)+ 哨兵 `tests/memory_contracts.rs::delete_hides_records_from_reads` | Passed |
| FC-MEM-CPLX-005 | CPLX | `iter(filter)`:时间 $O(N_c)$ 过滤 + $O(N_c\log N_c)$ RowId 排序;空间 $O(N_c)$(物化命中行的 `Arc` 句柄列表,**不复制记录体**;调用方收集的结果集另计) | 过滤/排序/墓碑哨兵 `tests/query_contracts.rs::iter_filters_and_sorts_by_rowid` | Passed |

#### 9.2.3 L2 持久层(persist)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-PERSIST-CPLX-001 | CPLX | WAL 提交:单条 $O(1)$ 内存追加;组提交 $N$ 条 $O(N)$ 追加 + **1 次** fsync/批;空间顺序写 | 操作计数单测 `tests/persist_contracts.rs::batch_insert_uses_single_fsync`(整批 Fsync 动作数 = 1) | Passed |
| FC-PERSIST-CPLX-002 | CPLX | WAL 回放(`wal::visit_frames`):时间 $O(\text{有效帧})$;空间 $O(1)$ 流式(批内帧缓冲 ≤ 单批帧数)。注:`Store::open` 当前将 WAL 整文件读入内存后再流式回放,该路径空间 $O(\text{WAL 字节})$(mmap 读路径优化已随 L3 落地;真正惰性驻留待 L5/L6 段句柄重构) | 解析证明(设计 04 §3.4;`wal::visit_frames` 逐帧回调不物化)+ 哨兵 `src/persist/wal/mod.rs::wal_replay_roundtrip` | Passed |
| FC-PERSIST-CPLX-003 | CPLX | CRC-32:时间 $O(n)$、空间 $O(1)$;$n$ = 字节数(实现为 `crc32fast` 查表/切片,常量因子依平台) | 解析证明(设计 04 §4.4;`crc32fast`)+ 哨兵 `src/persist/vsec.rs::vsec_detects_payload_corruption` | Passed |
| FC-PERSIST-CPLX-004 | CPLX | zone map 剪枝:时间 $O(\lceil n/1024\rceil \times \text{predicates})$;空间 16 B/块/字段(评估路径在 L4 查询层;L2 仅占位该空区,`zmap_len=0`) | 待补(L4) | Planned |
| FC-PERSIST-CPLX-005 | CPLX | bloom 判定:时间 $O(k)=O(7)$;空间 $1.44\log_2(1/p)$ bit/元素(评估路径在 L4 查询层;L2 仅占位该空区,`bloom_len=0`) | 待补(L4) | Planned |
| FC-PERSIST-CPLX-006 | CPLX | MANIFEST 提交:时间 $O(S_{\text{seg}})$ 写新文件;空间保留 2 版 | 解析证明(设计 04 §6;`manifest::encode` 逐段线性)+ 哨兵 `src/persist/manifest.rs::manifest_roundtrip` | Passed |
| FC-PERSIST-CPLX-007 | CPLX | `open` 恢复:时间 $O(\text{段总字节} + \text{WAL 字节})$(逐段读入并校验 + 回放);空间 $O(\text{段总字节} + \text{WAL 字节})$——L2 将段/WAL 整读入内存,mmap 读路径优化已随 L3 落地,真正惰性访问待 L5/L6 段句柄重构(设计 04 §11) | 哨兵 `tests/persist_contracts.rs::reopen_after_drop_recovers_from_wal`;解析证明(设计 04 §7;`read_segment_bytes` 逐段读入) | Passed |
| FC-PERSIST-CPLX-008 | CPLX | 单点写 `insert`:时间 $O(1)$ 内存 + WAL 追加(fsync 按 `FsyncPolicy`);空间 $O(d)$ | 哨兵 `tests/persist_contracts.rs::reopen_after_close_recovers_records`;解析证明(HashMap 追加 + 定长帧) | Passed |
| FC-PERSIST-CPLX-009 | CPLX | 单点读 `get(key)`:时间 $O(\log n)$ + 一次记录读(实现为 HashMap 期望 $O(1)$ $\subseteq O(\log n)$);`get_by_rowid`: $O(\log n)$ 版本链定位 | 哨兵 `tests/persist_contracts.rs::namespace_and_rowid_survive_reopen`;解析证明(设计 04 §5.5) | Passed |
| FC-PERSIST-CPLX-010 | CPLX | `as_of(t)`:时间 $O(V + N_c\cdot d)$($V$ = 全部物理版本数,`temporal::snapshot_at` 单遍择版本;当前实现不随段数$S_{\text{seg}}$分层加速);空间 $O(V + N_c)$(随历史窗口与候选规模增长) | 哨兵 `tests/persist_contracts.rs::version_chain_survives_reopen`;解析证明(设计 04 §5.5) | Passed |

#### 9.2.4 L3 索引层(index)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-INDEX-CPLX-001 | CPLX | HNSW 单点插入:时间 $O(d\cdot(ef_c\cdot M_0 + M\log_M N))$;构建 $O(N\cdot d\cdot ef_c\cdot M_0)$;空间 $\approx(8M+20)$ B/节点 + 边表。注:$M$/$M_0$ 为**有界常数**(≤4096,FC-INDEX-PRE-001);启发式选邻与修剪的 $M_0$ 多项式项(变量展开时最坏额外 $O(d\cdot M_0^3\cdot\log_M N)$)在最坏口径下含于系数,不影响 $N$/$d$/$\log N$ 的渐进结论 | 操作计数单测 `src/index/hnsw.rs::build_distance_calls_scale_linearly` | Passed |
| FC-INDEX-CPLX-002 | CPLX | HNSW 查询:期望上界 $O(d\cdot ef\cdot M_0)$(实测 $\approx(2\text{–}5)\cdot ef$ 次点积);空间期望 $O(ef\cdot M_0)$(`visited` 集合覆盖已展开节点的邻边)。注:本条目只约束索引内部;调用方 `memory::search` 当前的候选收集与 alive/过滤位图构造仍为 $O(N)$(L3 未做下推,L4 zone map/bloom 落地后消除,设计 05 §8) | 操作计数单测 `src/index/hnsw.rs::search_distance_calls_bounded_by_ef` | Passed |
| FC-INDEX-CPLX-003 | CPLX | 上层下降:时间期望 $O(d\cdot M\cdot\log_M N)$(依赖层级随机分布,§9.1 口径③);层高期望 $O(\log_M N)$ | 操作计数单测 `src/index/hnsw.rs::level_height_grows_logarithmically` | Passed |
| FC-INDEX-CPLX-004 | CPLX | hidx 编解码:时间 $O(\text{nodes}+\text{edges})$、空间 $O(\text{bytes})$ | 操作计数单测 `src/index/hidx.rs::hidx_encode_decode_scale_linearly` | Passed |

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
| FC-LIFE-CPLX-002 | CPLX | `retain` 扫描:时间 $O(N_{\text{cand}})$(元数据级,不读向量);空间 $O(N_{\text{cand}})$ | 解析证明(07 §3:单遍元数据扫描,不读向量、不重建索引)+ 哨兵 `tests/life_contracts.rs::retain_forgets_below_threshold` | Passed |
| FC-LIFE-CPLX-003 | CPLX | compaction 单轮:时间 $O(S_{\text{merge}}\cdot d\cdot ef_c\cdot M_0)$(建图主导);摊还 $O(d\cdot ef_c\cdot M_0\cdot W_{\text{amp}})$;空间峰值 $+O(S_{\text{merge}})$ | 待补 | Planned |
| FC-LIFE-CPLX-004 | CPLX | 活跃段数 $\le (T-1)\log_r(N/B)+c = O(\log_r N)$(I8);WAL $\le wal\_bytes$ | 待补 | Planned |
| FC-LIFE-CPLX-005 | CPLX | `snapshot`:时间 $O(1)$(clone `Arc` 视图);`backup_to`:同盘 $O(\text{files})$、跨盘 $O(\text{bytes})$;`check`: $O(\text{total bytes})$ | 解析证明(03 §4.3:快照 clone `Arc` 视图,与数据量无关;backup/check 逐文件/逐字节遍历)+ 哨兵 `tests/persist_contracts.rs::backup_is_independently_openable`、`tests/persist_contracts.rs::check_detects_corrupt_segment` | Passed |

#### 9.2.7 L6 量化层(quant)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-QUANT-CPLX-001 | CPLX | i8 量化点积:粗排副本带宽 $4d\to d$ B/行(÷4);VNNI 指令再 $\approx 4\times$;空间副本 $d$ B/行(f32 原向量另存) | 待补 | Planned |
| FC-QUANT-CPLX-002 | CPLX | 两阶段检索:粗排候选 $\le$ `rescore_candidates`(默认 4k);精排时间 $O(k\cdot d)$ | 待补 | Planned |

#### 9.2.8 记忆模型 / 安全 / 部署

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-MODEL-CPLX-001 | CPLX | `neighbors(from)`:时间 $O(\log E + degree)$;`predecessors(to)`:默认 $O(E)$ 全段扫描,`RelationIndex::Both` 时 $O(\log E + degree)$;空间 $O(degree)$。注:L1 内存实现为 `HashMap<RowId, Vec<Edge>>`,`neighbors` 期望 $O(1)$+degree(不劣于本条上界)、`predecessors` 全表 $O(E)$;`RelationIndex::Both` 反向索引自 L2 起生效,未落部分保持 Planned | 待补(L2 反向索引部分) | Planned |
| FC-MODEL-CPLX-002 | CPLX | `consolidate` 聚类(`score::cluster_by_similarity`):时间 $O(n^2\cdot d)$(候选 $n$ 两两余弦,并查集近似线性);空间 $O(n)$(并查集 + 分组)。候选规模受单库内存与 `ConsolidationPolicy.filter` 约束;渐进劣化须先改本条(FC-GLOBAL-CPLX-001) | 解析证明(两两比较循环可数)+ 哨兵 `tests/model_contracts.rs::consolidate_merges_cluster_and_keeps_sources` | Passed |
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
6. **L1 落地口径**:criterion 基准自 L3 起引入([14 §4](../design/14-testing.md));L1 的 CPLX
   以「操作计数单测 + 解析证明」验证,基准仅作后续回归哨兵。标记 `Passed` 的前提是
   验证方式已在「对应测试 / 基准」列显式登记,禁止用纯语义正确性测试冒充复杂度验证。

---

## 10. 追溯规则(FSVDD 强制)

1. **无孤儿实现**:任何新增业务逻辑必须在本矩阵登记至少一条约束;
2. **无失效契约**:本矩阵条目若与代码不符,以"先改契约、再改测试、再改代码"为准;
3. **无孤立测试**:每个测试文件头部注释必须引用其覆盖的 `FC-*` 编号;
4. **100% 映射**:本矩阵条目与其登记的测试一一对应,由 `tests/contract_traceability.rs`
   在 `cargo test` 中机械校验(无悬空引用、无孤立测试);
5. **豁免**:纯文档改动不新增契约;但涉及磁盘格式/API 语义的文档改动必须先更新本矩阵;
6. **证伪原则落地口径**:每条 ERR / INV 契约均配备专项失败测试——放宽任一约束
   (如 varint 超长校验、余弦 ε 阈值、`Dimension` 边界)必有一个测试变红;
   机械化变异测试(`cargo-mutants`)列入 L2 阶段 CI 任务(见 [14 §7](../design/14-testing.md));
7. **复杂度可追溯**:每条 `CPLX` 契约必须有对应的基准 / 操作计数测试(§9.3);
   任何使复杂度渐进退化的改动,必须先更新 `CPLX` 契约,再改测试与代码。
