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
| 2026-09 | L5 第四轮独立审查整改:① **修复损坏隔离段被 compaction 清除**——恢复期把跳过段记入 `WriterState.unavailable_segments`,compaction 计划排除(否则自动维护会把可修复的损坏段合并、移 trash 并 purge,`check()` 反绿,`FC-PERSIST-ERR-006`);② **区级结构预校验**——版本表/关系区/delta 区畸形在非 fail-fast 下按段隔离(不再整体拒启),fail-fast 错误补段号;③ 关系语义版本策略修正:`FLAG_FULL` 属语义变化,按设计 04 §12 升**主版本 1**(`0x0100`),主版本 0 旧读者拒绝新库而非误读(`MAX_MAJOR=1`);④ `maintenance_tick` 只读跳过 compaction;`run_retain` 不再静默吞错(后台尽力、手动传播);MANIFEST 裁剪失败降级(提交点后不返错);⑤ 补回归:旧格式段(无 `FLAG_FULL`)关系 upsert、段号升序回放、touch+update 同批物化不重复累加、损坏段不被 compaction 清除;⑥ 修正注释反向(`edges.rs`/`codec.rs`/`open.rs`)与文档漂移 |
| 2026-09 | L5 第三轮独立审查整改:① **修复旧格式段关系语义误判**——此前把所有 < 0x0004 段当作全量重置,旧库增量段(空关系表)应用时会清掉先前段的边;现仅带 `FLAG_FULL` 的段重置,旧段一律 upsert(已发布旧库恒单段,重置与 upsert 等价;`FC-MODEL-POST-007`);② **修复损坏段隔离导致后续拒启**——非 fail-fast 跳过时不再把损坏段移入 `trash/`(MANIFEST 仍引用,移动后重开会因引用缺失拒启、隔离文件更被 purge 删除),改为原地保留、内存跳过、`check()` 报告(`FC-PERSIST-ERR-006`);③ 新增 `Mneme::maintenance_tick()` 手动执行一轮后台维护,维护相关测试不再依赖后台线程调度(消除真实时间 flaky,`FC-LIFE-CPLX-006`);④ 补回归:latest 入 keep 时丢弃被版本行覆盖的 Access delta、flush 后注册的命名空间崩溃不丢(两条变异存活缺口);⑤ 收尾:物理回收时同步清 `access`/`access_dirty`、`last_access_ms` 取 max、恢复按段号升序回放、`plan::compile` 测试探针收敛;⑥ 文档同步(04 §7 损坏段原地保留、16 `maintenance_tick`) |
| 2026-09 | L5 第二轮独立审查整改:① **修复恢复后未回填槽位归属**——重开后 `unpersisted_slots` 把全部已落盘槽位当未落盘,空 flush 产段、compaction 以空段替换活跃段集永久丢数据(`FC-PERSIST-POST-012` 新增);② **修复 compaction 丢 delta 与关系复活**——`Access` delta 未覆盖的 RowId 增量随新段携带,关系区引入**全量/增量标志**(段格式次版本升至 `0x0004`,旧段按全量处理),恢复时全量段先重置再应用(`FC-PERSIST-POST-010`、`FC-MODEL-POST-007` 收紧);③ **修复跨段版本链死比率空转**——`segment_dead_ratios` 与 `select_survivors` 共用同一回收口径(含整链范围判定),不再零回收无限重写(`FC-LIFE-INV-008`);④ **修复 `access_dirty` 误清**——仅当 RowId 最新版本本次被物化才清;⑤ 版本行 `access` 列改为**始终**写出累计快照(缺省 0),消除旧版本值 + delta 的重复累加;⑥ **修复残留旧 WAL 复活已注销命名空间**——注册/注销帧分配真实 seqno 并统一受水位约束,旧格式 seqno=0 metadata 帧仅 `watermark == 0` 时应用(`FC-PERSIST-POST-011`、`FC-LIFE-POST-006`);⑦ 时钟回拨集成测试改跨 open 构造(原测试被 `MonotonicClock` 钳平,为真空测试);⑧ 维护句柄计数、多段恢复索引区校验、fsck 建议口径、`SnapshotStats` 过滤回收槽位 |
| 2026-09 | L5 合并前独立审查整改:① **修复 delta 恢复重复累加**——恢复期 `Access` delta 仅当 `seqno` 晚于该 RowId 最新版本行时才累加,否则版本行的累计快照已包含该增量(`FC-PERSIST-POST-010` 收紧,新增恒等回归);② **修复 `encode_delta` 条目数静默截断**——超 `u16::MAX` 由计数钳制改为 `LimitExceeded`(`FC-PERSIST-ERR-011`,补 65535/65536 边界);③ **修复死比率无限重写**——默认 `horizon = None` 时无可回收死行,统计返回空、不再反复重写段(`FC-LIFE-INV-008`,补哨兵);④ **修复 compaction 提交后半同步**——MANIFEST 提交后旧段清理失败降级为孤儿文件(下次启动清理),先对齐内存视图再尽力清理,绝不报错(`FC-LIFE-ERR-001`);⑤ WAL 重置改为「先写头、后截断」并接 `FsyncHook`,失败重建、再失败停用句柄,旧轮转文件删除失败不阻断(`FC-PERSIST-INV-005`/`FC-PERSIST-POST-002`);⑥ `CompactionState` 落地 `Paused` 态与 `pause`/`resume` 状态转移(`FC-LIFE-STA-001`);⑦ 整链回收改为「最新墓碑/过期在窗口外 ⇒ 整链回收」,消除时钟回拨下 `latest` 悬挂/删除复活(`FC-MODEL-POST-004`);⑧ 维护句柄计数修正,最后一个库句柄 Drop 时同步 stop+join;⑨ 补测试:空 flush 无操作、Checkpoint 删旧 WAL、硬链接回退复制、运行中暂停中止、错误变体断言;`FC-PERSIST-CPLX-011` 以解析证明 + 哨兵转 `Passed`;⑩ 一致性小项:关系区计数改 `LimitExceeded` 且解析拒绝区尾残留、`drop_namespace` 按最新版本去重(不重复烧 `seqno`)、访问缓冲加条目上限、Checkpoint 重置失败不阻断已提交 flush、死比率选段同比率按段号确定,并按 rust 规范拆分超长函数、收敛参数(`Store::compact`/`next_manifest_after_merge`/`decode_delta`/`encode_delta`/`select_survivors`/`Mneme::compact`/`check`/维护循环) |
| 2026-09 | 落地 L5 生命周期层:① 增量段 flush(只物化未落盘槽位)+ 多段恢复(每段重排映射)+ 多图 ANN 归并;② size-tiered compaction(段组替换、`history_horizon` 整链回收、崩溃原子)与 `CompactionControl` 状态机;③ msec `ttl_map`(zmap 区尾,`FORMAT_VERSION` 次版本升至 `0x0003`,旧段尾长 0 兼容)与块级 TTL 剪枝;④ `delta` 区(访问/关系变更)与 WAL 轮转/Checkpoint;⑤ 后台维护线程(读命中攒批落 WAL、自动遗忘默认关闭、自动 compaction);⑥ 命名空间路径规范化与 `NsUnregister` 帧;⑦ `SnapshotStats`、硬链接备份、死比率与 fsck 合并建议;⑧ `RelationIndex::Both` 反向表落盘。新增 `tests/l5_contracts.rs`(24 用例)并登记追溯门禁;`FC-LIFE-*`/`FC-PERSIST-*`/`FC-MODEL-*` 相关条目标 `Passed`;已知取舍:压缩后内存槽位从版本链剪除但不重排(零拷贝段句柄属后续层) |
| 2026-09 | 启动 L5 生命周期层:① 新增多段 size-tiered compaction、增量段 flush、msec `delta` 区、WAL 轮转/Checkpoint、TTL 块剪枝、后台维护线程、命名空间规范化与注销、快照统计、硬链接备份、fsck 死比率、反向关系表等契约条目(`FC-LIFE-POST-003..009`、`FC-LIFE-STA-001`、`FC-LIFE-ERR-001`、`FC-LIFE-CPLX-006`、`FC-PERSIST-POST-010/011`、`FC-PERSIST-STA-004`、`FC-PERSIST-ERR-011`、`FC-PERSIST-CPLX-011`、`FC-MODEL-POST-007`);② 改写 `FC-PERSIST-POST-002/008` 与 `FC-MODEL-CPLX-001` 中「L5 未落」措辞;③ 状态先 `Planned`,随里程碑回填真实测试路径 |
| 2026-09 | L4 第五轮独立审查整改(第三轮复审,修第四轮回归):① 修复 `observe_meta` 整跳保留名子树引入的嵌套路径漏观察——metadata `{"key": {"x": 5}}` 下 `key.x` 行级可命中,但 zone map 未观察,遇字面点分键 `"key.x"` 注册后块被误剪;现保留名只拦自身注册、对象子路径照常递归观察,新增 zone 层与计划器等价性回归(`FC-QUERY-POST-005`);② `resolve` 与 `is_reserved_field` 的保留名清单收敛为 `RESERVED_FIELDS` 常量并加同步测试;③ ISO 时/分/秒收紧为恰好 2 位(同契约),范围采样公式改 `span * index / 100` 消除先除后乘截断;④ 文档:`01` 运维报告标注措辞修正、`16` 压缩行补「L11 落地」注、`docs/rust` 两处 `clock.rs` 引用偏位修正 |
| 2026-09 | L4 第四轮独立审查整改(第二轮复审):① **修复保留字段被同名 metadata 影子化导致的计划器漏报**——`ZoneIndex::observe_meta` 此前会把 metadata 里的 `key`/`rowid`/`__ns`/`access_count`/`last_access` 注册进 zone map,而 `pred_eval::resolve` 行级以保留值为准,这些字段所在块会被误剪(`exists(key)`/`rowid > N` 静默少结果);现新增 `pred_eval::is_reserved_field` 单源,`observe_meta` 一律跳过保留名,回归覆盖 zone 层与计划器等价性(`FC-QUERY-POST-005`);② `decode_inverted` 拒绝 `doc_len = 0`(避免 `avgdl = 0` 使 BM25 分数 NaN,`FC-PERSIST-ERR-010`);③ ISO 时区收紧为恰好 2 位时分,补小写 `t`/`z`、紧凑偏移与全范围采样往返测试(`FC-QUERY-POST-007`);④ 契约补登记 `tokenize` 纯标点、BM25 只计活行、融合极端极差、字段字典 `key` 唯一等测试;⑤ 追溯门禁 `test_fns` 跳过注释/属性行,`src/persist/flush.rs` 纳入 `SOURCES`;⑥ 文档:修正 `01` 运维报告标注范围、`03` 模块清单补 `analysis/`、校准 `docs/rust` 行号引用(46 行/79 处,其余仍对齐)与 `msec/decode.rs` 区数注释;`build_ns_stats` 口径注释同步 |
| 2026-09 | L4 第三轮独立审查整改:① **修复 `query::iso::parse_fraction` 毫秒补零 bug**——小数秒不足 3 位时越过已确认数字读入后续字符(`.5Z` 算成 920ms),`debug` 构建对 `.1-05:00` 触发减法溢出 panic;现按已确认位数截断补零,秒域收紧为 0–59(拒绝闰秒 60),新增定向回归与 ISO 往返契约 `FC-QUERY-POST-007`;② 契约漂移修复:`FC-PERSIST-ERR-009` 正文同步「单段恢复不要求 `hidx`、无 `hidx` 不再豁免」;`FC-QUERY-CPLX-003` 空间上界由 $O(k)$ 校正为命中并集 $O(\min(N_{ns}, \sum df_t))$;③ 覆盖补齐:新增 `FC-CORE-POST-008`(`tokenize` 切词口径),`src/core/text.rs`/`src/query/iso.rs`/`src/memory/analysis/inv.rs` 纳入追溯门禁 `SOURCES`;④ 追溯门禁加固:孤立测试判定改用契约行「对应测试」列(防正文随意提及放水)、`fn_name` 支持 `pub`/`async` 前缀、新增 `tests/` 下 `*_contracts.rs` 登记自检;⑤ 测试补强:非恒等重排映射重开端到端(BM25 分数逐位一致)、msec 畸形区补尾残留/非法 `bit_len`/`k` 定向用例与任意字节 proptest、bloom 否定补行级求值探针(证伪「未走预筛」)、`Weighted` 极端极差有限性;⑥ Rust 规范整改:`fusion` min-max 归一改 `f64` 中间量防 `inf/inf`、字段字典去重 `key`、`in` 列表去重注明 $O(n^2)$ 规模、局部变量命名与注释同步、ISO 魔数常量化;⑦ 文档同步:16 未落地 API 标注(`SnapshotStats`/`RelationKind::custom`)、03 模块清单去除已迁移的 `search_exec.rs`、06 §2 计划器正文按实现(每查询一份/`Plan` 形状/`Not` 不取反/无选择性重排)同步、README feature 表标注未定义、16 panic 例外措辞与 `fsync_hook` 补录;⑧ `SnapshotNamespace` 五方法补「过期以快照时刻 `as_of_ms` 判定」rustdoc |
| 2026-09 | 落地 L4 检索层:① 新增 `src/query/`(过滤 DSL 解析/`Display`/JSON 往返、zone map+bloom 块级计划器、BM25 两遍全局统计、RRF/加权融合、`SearchBuilder::execute` 执行管线),`execute` 与内部阶段自 L1 迁至 L4 以保持 L0→L6 单向依赖;② 新增 `src/core/text.rs` 分词(空白切词 + CJK bigram + 停用词)与库根 `filter!` 宏,`Expr` 提供 `from_str`/`Display`/`to_meta`/`from_meta`;③ L1 新增 `memory/analysis/`(内存倒排/zone map/bloom,写路径增量维护,`WriterState`/`ReaderView` 以 `Arc` 随快照携带,支持 `as_of`);④ L2 `flush` 实际写入 msec 的 `field_dict`/`zmap`/`bloom`/`inverted` 四区(`FORMAT_VERSION` 次版本升至 `0x0002`),`open` 校验区结构并从磁盘倒排经"段内槽位→全局槽位"重排映射直接重建(`build_remap` 不再要求 hidx),zone map 从槽位重建(等价);⑤ 语义决策:`Not` 不做块级取反(三值语义下位图取反会漏,交行级残差),`Fusion` 单通道设置与 `Weighted.alpha` 越界/非有限 → `Config`;⑥ 契约:新增 `FC-QUERY-POST-002..005`、`FC-PERSIST-POST-008`,转正 `FC-QUERY-ERR-001`、`FC-QUERY-CPLX-001..004`、`FC-INDEX-INV-005/006/021`、`FC-PERSIST-CPLX-004/005`,更新 `FC-MEM-ERR-002` 与 §0.2 错误矩阵;新增 `tests/l4_contracts.rs` 并登记追溯门禁 |
| 2026-09 | L4 合并前独立审查整改:① zone map 下推修复三处静默漏报——`exists` 只认数值/null 会剪掉字符串/布尔/null 行、字段类别被同名 metadata 数字污染后字符串比较返回空位图、>2^53 整数经 `f64` 舍入误剪;现引入 `BlockStat.has_any`,类型不匹配/超精确范围一律返回全 1,超范围整数区间放宽为 ±∞(`FC-QUERY-POST-005`);② 解析器拒绝非有限数字与超范围相对时间(`checked_add` + ISO 可往返范围,`FC-QUERY-ERR-001`),`Display` 空逻辑项规约为 `always`/`never`,JSON 解码拒绝空数组;③ `as_of` 历史检索的 TTL 以视图时刻为准(`FC-QUERY-POST-006`);④ 分词停用词开关写入 MANIFEST 并在 `open` 时锁定(`FC-PERSIST-POST-009`),`bloom_fpp`/`field_dict_max` 建库校验(`FC-INDEX-PRE-001`);⑤ 旧格式段(四区为空)显式降级重建而非 `Corrupted`(`FC-PERSIST-POST-008`),新增 `FC-PERSIST-ERR-010` 畸形轻量索引区条目,msec 倒排 offset/length 与词频合并溢出改 `checked` 拒绝、倒排区新增 `[u64 postings_total_len]` 显式边界(自本分支 0x0002 起;此前 0x0002 开发期段按结构校验拒绝并降级重建);⑥ Rust 规范整改:`parse.rs` 拆为 `parse/{mod,literal}.rs`、`msec/index.rs` 拆出 `msec/inverted.rs`,`from_meta`/倒排编解码/融合/通道参数收敛,`WriterState.indexing_paused` 加 `is_` 前缀,模块注释同步;⑦ 测试与契约补强:计划器漏报回归、BM25 精确分数、过滤先行证伪、stopwords 锁定、追溯门禁按路径配对与反引号编号解析 |
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
| `Corrupted { segment, reason }` | CRC/魔数不符等数据损坏(msec `delta` 区畸形见 FC-PERSIST-ERR-011) | FC-CORE-ERR-001、FC-INDEX-ERR-001、FC-INDEX-ERR-002、FC-PERSIST-ERR-011 | `segment=None` 表示文件级损坏 |
| `DimensionMismatch { expected, got }` | 向量长度 ≠ 建库维度(写/查) | FC-GLOBAL-PRE-001、FC-MEM-PRE-001/004 | |
| `MetricMismatch { existing, requested }` | 打开时度量与库中记录不符 | — | 保留,L2 起使用 |
| `KeyMismatch { expected, got }` | `supersede` 的新记录自带 key 与目标 key 冲突(信念修订须沿用同一 key) | FC-MODEL-POST-003 | 新记录省略 key 时继承目标 key |
| `KeyNotFound(Key)` | 键不存在 | — | 保留,当前无 API 产生 |
| `DuplicateKey(Key)` | `InsertMode::RejectDuplicate` 命中,或写版本携带的 key 已被另一可见记录占用(key 迁移冲突) | FC-MEM-POST-001、FC-MEM-POST-007 | |
| `FilterParse(String)` | 过滤 DSL 语法错误(带位置) | FC-QUERY-ERR-001 | L4 已使用 |
| `Busy(&'static str)` | 独占锁被占/备份中 | — | 保留 |
| `TooLarge { field, limit, got }` | 字段载荷超限额(key/text/meta 字节) | FC-GLOBAL-PRE-003、FC-MEM-PRE-002 | |
| `UnsupportedVersion { file, found, max }` | 文件主版本过新 | FC-PERSIST-ERR-002、FC-INDEX-ERR-001 | L2 起使用 |
| `Closed` | 库已关闭后经任意句柄读写 | FC-MEM-ERR-001、FC-MEM-STA-001 | 原 `Invalid("closed")` |
| `NonFinite` | 向量分量或 `importance`/`confidence`/边权/`boost` 等数值输入含 `NaN`/`±Inf`(会污染打分、遗忘公式与排序) | FC-GLOBAL-PRE-002、FC-GLOBAL-PRE-004、FC-MEM-PRE-001/003 | 原 `Invalid("向量分量必须是有限值")` |
| `LimitExceeded { field, limit, got }` | 参数越上限(维度、`top_k`、`ef`、`ef_search`、HNSW 度数) | FC-CORE-PRE-001、FC-GLOBAL-PRE-004、FC-MEM-PRE-003、FC-INDEX-PRE-001 | 原 `Invalid("top_k 超过上限")` 等 |
| `MetaTooDeep { limit, got }` | metadata 嵌套深度超限 | FC-GLOBAL-PRE-003、FC-MEM-PRE-002 | 原 `Invalid("metadata 嵌套过深")` |
| `Config { reason }` | 建库/查询配置非法(缺维度、无查询通道、MMR `lambda` 非有限值、`Fusion` 未同时启用双通道、`Weighted.alpha` 越界或非有限)、策略参数含非有限值(`min_importance`/`access_weight`/`threshold`/`dedup_threshold`)或越界(`dedup_threshold`/`threshold` ∉ [0,1])或非法(`max_cluster = 0`)、HNSW 参数域非法(`m < 2`/`m0 < m`/`ef_construction = 0`/`ef_search = 0`/过滤阈值越界或 `brute > post`)、命名空间路径深度超 `Limits.ns_depth` 或含非法字符(首次写入时,`namespace()` 本身不返回 `Result`) | FC-MEM-STA-001、FC-MEM-PRE-003、FC-MODEL-POST-006、FC-LIFE-POST-002、FC-LIFE-POST-005、FC-GLOBAL-PRE-004、FC-INDEX-PRE-001、FC-MEM-ERR-002 | 原 `Invalid("新建内存库必须指定维度")` 等 |
| `Unsupported { feature }` | 能力延后到后续层,绝不静默降级(持久库 `backup_to` 已在 L2 落地;只读模式写亦返回本变体;L4 起 `text`/`Fusion` 已实现,见 FC-MEM-ERR-002) | FC-MEM-ERR-002、FC-PERSIST-ERR-003 | 只读写、**纯内存库** backup |
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
| FC-CORE-POST-008 | POST | `tokenize` 分词口径(设计 06 §3.5):按 Unicode 空白分段并去首尾非字母数字;非 CJK 段整词小写化;连续 CJK 段切 bigram(单字保留);`stopwords_enabled` 控制内置停用词过滤;空白/纯标点输入 → 空 `Vec`,输出保序 | `src/core/text.rs::latin_words_are_lowercased_and_punctuation_trimmed`、`src/core/text.rs::cjk_runs_become_bigrams`、`src/core/text.rs::single_cjk_char_is_kept`、`src/core/text.rs::mixed_script_splits_at_script_boundary`、`src/core/text.rs::stopwords_are_filtered_only_when_enabled`、`src/core/text.rs::whitespace_only_text_yields_no_tokens`、`src/core/text.rs::punctuation_only_text_yields_no_tokens` | Passed |
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
| FC-MEM-ERR-002 | ERR | 尚未落地或与当前形态不符的能力以结构化错误返回、绝不静默:**纯内存库** `backup_to` → `Unsupported`(持久库已在 L2 实现,见 `FC-PERSIST-POST-004`);`Fusion` 未同时启用向量与文本通道 → `Config`(设置即拒绝,不静默忽略);`Scoring::bias_routing = true` → `Unsupported`(HNSW 启发式路由未落地,设计 10 §2.3);`text`/`Fusion` 能力本身已在 L4 落地(`FC-QUERY-POST-004`);`open`/`path` 已在 L2 落地(新建持久库缺维度返回 `Config`,不再是 `Unsupported`) | `tests/life_contracts.rs::deferred_features_return_structured_errors`、`tests/l4_contracts.rs::hybrid_fusion_and_validation`、`tests/l4_contracts.rs::bias_routing_unsupported_is_explicit` | Passed |

---

## 2. 持久层(persist)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-PERSIST-INV-001 | INV | **I1**:已确认写入不半写;未确认写入重启后要么完整可见要么不存在;写事务 append/sync 失败时截断半写帧(失败写绝不持久、不遮挡后续已确认写) | `tests/persist_contracts.rs::reopen_after_close_recovers_records`、`tests/persist_contracts.rs::reopen_after_drop_recovers_from_wal`、`tests/persist_contracts.rs::injected_wal_failure_keeps_confirmed_prefix`、`tests/persist_contracts.rs::injected_fsync_failure_rolls_back_frames` | Passed |
| FC-PERSIST-INV-002 | INV | **I2**:任意 bit 损坏可检出或拒绝启动,绝不静默返回错误数据 | `src/persist/vsec.rs::vsec_detects_header_corruption`、`src/persist/vsec.rs::vsec_detects_payload_corruption`、`src/persist/manifest.rs::manifest_detects_header_corruption`、`src/persist/manifest.rs::manifest_detects_payload_corruption`、`src/persist/wal/mod.rs::wal_bad_crc_stops_replay`、`tests/persist_contracts.rs::verify_on_open_detects_payload_corruption`、`tests/persist_contracts.rs::check_detects_corrupt_segment` | Passed |
| FC-PERSIST-INV-003 | INV | **I3**:活跃段集合 = 某 MANIFEST 版本所列集合 | `tests/persist_contracts.rs::flush_checkpoints_after_materialize` | Passed |
| FC-PERSIST-INV-004 | INV | **I4**:WAL 总量 ≤ `wal_bytes`;段文件只增不改 | `tests/persist_contracts.rs::wal_capacity_triggers_snapshot_flush`、`tests/persist_contracts.rs::committed_segment_is_write_once` | Passed |
| FC-PERSIST-INV-005 | INV | **I1(撕裂尾部)**:可写重开时物理截断 WAL 撕裂尾部(有效长度 = 头部 + 完整帧),使截断之后的追加写入不被残尾永久屏蔽;有效前缀不受影响;WAL 短于文件头(Checkpoint 重置中途崩溃)视为撕裂头并重建,不拒绝打开;重置/回滚写头与 fsync 经 `FsyncHook` 可注入,重置写头失败时重建、重建仍失败则停用句柄(`Io`),绝不向状态可疑的文件继续追加 | `tests/persist_contracts.rs::torn_wal_tail_truncated_on_reopen`、`tests/persist_contracts.rs::short_wal_header_is_recreated_on_open`、`src/persist/store/wal_writer.rs::failed_reset_poisons_writer` | Passed |
| FC-PERSIST-INV-006 | INV | **I1(提交点)**:WAL 帧成功 fsync 即为提交点;其后后台 flush 失败不回滚已提交写,绝不出现「返回 `Err` 但重启后可见」的矛盾(flush 失败由下次写重试,`stats().wal_bytes` 可观测) | `tests/persist_contracts.rs::flush_failure_does_not_lose_committed_write` | Passed |
| FC-PERSIST-INV-019 | INV | **I19(记录级)**:`delete`/`update` 返回 `Ok` 后,崩溃 + WAL 截断仍生效,删除永不复活(`touch`/`relate` 的 WAL 帧见 FC-PERSIST-POST-005) | `tests/persist_contracts.rs::delete_survives_flush_and_reopen`、`tests/persist_contracts.rs::crash_after_delete_does_not_resurrect`、`tests/persist_contracts.rs::update_survives_reopen` | Passed |
| FC-PERSIST-INV-020 | INV | **I20**:`path↔NsId`、`next_ns_id`、`next_rowid` 可由 MANIFEST+WAL 重建,ID 永不复用 | `tests/persist_contracts.rs::namespace_and_rowid_survive_reopen` | Passed |
| FC-PERSIST-POST-001 | POST | **I15**:`insert_batch` 整批原子:可见记录数 ∈ {0, n},无部分批 | `tests/persist_contracts.rs::batch_insert_is_atomic_across_reopen` | Passed |
| FC-PERSIST-POST-002 | POST | Checkpoint 仅当 `seqno ≤ watermark` 的覆盖条目已物化(写入增量段或 `delta` 区)时才截断/删除 WAL;重置按「先写完整头、后截断」执行(重置后文件恰为文件头长且头可解析);重置失败不阻断已提交的 flush(段/MANIFEST 已提交,旧帧均 ≤ watermark),句柄安全由 `reset` 内部重建/停用保证;WAL 轮转后按文件序回放、`seqno ≤ watermark` 的帧跳过,单批不跨文件(FC-PERSIST-POST-011) | `tests/persist_contracts.rs::flush_checkpoints_after_materialize`、`src/persist/store/wal_writer.rs::reset_keeps_complete_header` | Passed |
| FC-PERSIST-POST-003 | POST | **I16**:`close()` 返回 `Ok` 后所有已确认写入持久;`Drop` 不保证 | `tests/persist_contracts.rs::reopen_after_close_recovers_records` | Passed |
| FC-PERSIST-POST-004 | POST | `backup_to` 先 flush 再复制段(vsec/msec/hidx)/MANIFEST/WAL,`current` 最后写;产物可独立 `open`(设计 16 §7) | `tests/persist_contracts.rs::backup_is_independently_openable` | Passed |
| FC-PERSIST-POST-005 | POST | `touch`/`relate`/`unrelate` 的 WAL 帧持久性:崩溃后回放 `TouchRow`/`Relate`/`Unrelate` 帧,`access_count`/访问时刻与关系边不丢失(importance 强化随版本 `Insert`)、unrelate 不复活 | `tests/persist_contracts.rs::relate_and_unrelate_survive_crash`、`tests/persist_contracts.rs::touch_boost_survives_crash`、`src/persist/wal/mod.rs::wal_touch_relate_roundtrip` | Passed |
| FC-PERSIST-POST-006 | POST | WAL `Insert`/`DeleteRow` 帧携带版本事务时间 `tx_ms`;崩溃恢复后 `as_of(t)` 历史正确(删除前时点可见、删除后不可见),不以记录体 `created_at` 或 `0` 代替 | `tests/persist_contracts.rs::as_of_history_survives_reopen` | Passed |
| FC-PERSIST-ERR-001 | ERR | 未知 WAL 帧类型 → 停止回放并报错,不静默跳过 | `src/persist/wal/mod.rs::wal_unknown_frame_type_errors` | Passed |
| FC-PERSIST-ERR-002 | ERR | 更高主版本 → `UnsupportedVersion`(I18),段级过新版本即使默认非 fail-fast 也拒绝打开、绝不降级为跳过 | `src/persist/vsec.rs::vsec_rejects_higher_major`、`src/persist/manifest.rs::manifest_rejects_higher_major`、`tests/persist_contracts.rs::higher_major_segment_is_rejected` | Passed |
| FC-PERSIST-ERR-003 | ERR | 只读模式写操作 → `Unsupported { feature: "只读模式写入" }`,绝不静默;只读打开不创建/改写 WAL(设计 04 §13);只读打开绝不改动文件系统(不建目录、不清 `trash/`),库目录不存在 → `Config` | `tests/persist_contracts.rs::read_only_rejects_writes`、`tests/persist_contracts.rs::read_only_open_does_not_create_wal`、`tests/persist_contracts.rs::read_only_open_does_not_mutate` | Passed |
| FC-PERSIST-ERR-004 | ERR | 打开时显式维度与 MANIFEST 不符 → `DimensionMismatch`,拒绝打开(设计 16 §3) | `tests/persist_contracts.rs::dimension_mismatch_rejected_on_open` | Passed |
| FC-PERSIST-ERR-005 | ERR | 目录状态不一致(`current` 存在但无合法 MANIFEST,或存在段文件却既无 MANIFEST 也无 WAL)→ `Corrupted`,绝不当作新库覆盖既有数据(设计 16 §3)。注:段文件存在但**有 WAL** 属首次 flush 崩溃,见 `FC-PERSIST-STA-003`,不返回 `Corrupted` | `tests/persist_contracts.rs::corrupt_current_without_valid_manifest_is_rejected`、`tests/persist_contracts.rs::segments_without_manifest_are_rejected` | Passed |
| FC-PERSIST-ERR-006 | ERR | MANIFEST 引用的段文件缺失或为空 → `Corrupted`,绝不静默跳过而少返回数据(I2/I3);**损坏段(文件存在但解析失败或区级结构畸形)在非 fail-fast 下仅在内存跳过、文件保持原地**(绝不自动移入 `trash/`,否则 MANIFEST 引用缺失会使后续打开拒启)、可再次打开、`check()` 报告,**且 compaction 计划必须排除隔离段**(绝不把损坏段当活跃段合并清除) | `tests/persist_contracts.rs::referenced_segment_missing_is_rejected`、`tests/persist_contracts.rs::referenced_segment_empty_is_rejected`、`tests/l5_contracts.rs::skipped_corrupt_segment_keeps_library_openable`、`tests/l5_contracts.rs::corrupt_segment_is_never_compacted_away` | Passed |
| FC-PERSIST-ERR-007 | ERR | WAL `BatchCommit` 的批内帧计数与 `batch_crc` 在回放时校验;不符 → `Corrupted`,拒绝应用半批,绝不静默(I15);未闭合批(缺 `BatchCommit`)不计入已提交长度,重开时截断,绝不吞掉其后单操作事务 | `src/persist/recover/replay.rs::replay_rejects_mismatched_batch_crc`、`src/persist/recover/replay.rs::replay_rejects_mismatched_batch_count`、`src/persist/recover/replay.rs::replay_applies_well_formed_batch`、`src/persist/recover/replay.rs::unclosed_batch_is_not_committed`、`tests/persist_contracts.rs::unclosed_batch_tail_does_not_swallow_later_writes` | Passed |
| FC-PERSIST-ERR-008 | ERR | 独占锁基于 OS 咨询锁(`std::fs::File::try_lock`):活实例持有 → `Busy`;进程崩溃/退出时内核自动释放,后续实例无需租约/接管即可获取;`Drop` 释放锁但不删除锁文件,避免不同 inode 各自加锁破坏互斥(设计 16 §3) | `src/persist/storage.rs::file_lock_blocks_second_holder`、`src/persist/storage.rs::file_lock_acquires_when_lock_file_exists`、`src/persist/storage.rs::file_lock_file_persists_after_drop` | Passed |
| FC-PERSIST-ERR-009 | ERR | 单段恢复时的槽位重排映射(`recover::state::build_remap`,倒排载入与 hidx 载入共用,故**不要求 `hidx`**):版本行槽位越界、重复,或存在未被任何版本行引用的段内槽位(vsec/msec 行数不一致)→ `Corrupted`,绝不静默把未引用槽位映射到槽位 0;多段场景逐段独立映射(`build_remaps` 返回各段映射;存在跳过段时对应段不入映射);无 `hidx` 不再豁免,段内槽位不一致同样 → `Corrupted`;载入期二次校验:重排映射指向不存在槽位 → `Corrupted` | `src/persist/recover/state.rs::build_remaps_maps_slots_in_version_chain_order`、`src/persist/recover/state.rs::build_remaps_rejects_out_of_range_duplicate_or_unreferenced_slots`、`src/persist/store/open.rs::load_index_rejects_remap_past_state_slots` | Passed |
| FC-PERSIST-ERR-010 | ERR | msec 轻量索引区结构畸形(字段类别未知、区尾残留、倒排 offset/length 越界或相加溢出、词频合并溢出、bloom 位数/哈希数非法、doc 区 `doc_len = 0`(会使 BM25 `avgdl = 0` 产生 NaN 分数)等)→ `Corrupted`,绝不 panic / 回绕 / 静默截断;fail-fast 打开时上报,非 fail-fast 时降级全量重建(索引是加速器而非数据源) | `src/persist/msec/index.rs::malformed_regions_are_rejected`、`src/persist/msec/inverted.rs::malformed_inverted_is_rejected_without_panic`、`src/persist/recover/state.rs::malformed_region_section_is_error`、`src/persist/msec/inverted.rs::decode_inverted_never_panics_on_arbitrary_bytes`、`src/persist/msec/index.rs::decode_regions_never_panics_on_arbitrary_bytes` | Passed |
| FC-PERSIST-ERR-011 | ERR | msec `delta` 区结构畸形(kind 未知、长度越界或相加溢出、条目区 CRC 不符、区尾残留)→ `Corrupted`,绝不 panic / 回绕 / 静默截断;编码条目数超 `u16::MAX` → `LimitExceeded`,绝不钳制计数;非 fail-fast 打开时该段按损坏跳过或降级重建,绝不返回错误数据 | `src/persist/msec/delta.rs::malformed_delta_is_rejected`、`src/persist/msec/delta.rs::delta_count_overflow_is_rejected` | Passed |
| FC-PERSIST-STA-001 | STA | 段生命周期:`Building → Committed → Obsolete → (trash)`;`Committed` 段内容不可变(write-once,重写产生新段) | `tests/persist_contracts.rs::committed_segment_is_write_once` | Passed |
| FC-PERSIST-STA-002 | STA | 崩溃点状态:`Building` 段(`.tmp` 半成品)与 MANIFEST 未引用的段为孤儿,可写打开时清理;不进入任何 MANIFEST 视图 | `tests/persist_contracts.rs::orphan_tmp_cleaned_on_open`、`tests/persist_contracts.rs::unreferenced_segment_cleaned_on_open` | Passed |
| FC-PERSIST-STA-003 | STA | 首次 flush 中途崩溃(段已写、MANIFEST 未提交):存在 WAL 时以 WAL 为准重建,孤儿段被清理,绝不误判为 `Corrupted` 而丢数据 | `tests/persist_contracts.rs::first_flush_crash_recovers_from_wal` | Passed |
| FC-PERSIST-STA-004 | STA | 多段 MANIFEST 提交:增量段 **append** 与 compaction 组 **replace** 均先写新 `MANIFEST.<v>` 再原子换 `current`;任意时刻活跃段集合 = 某 MANIFEST 版本所列集合;提交失败不改变活跃集合,已提交段 write-once | `tests/l5_contracts.rs::incremental_flush_appends_segments`、`tests/l5_contracts.rs::compaction_bounds_segment_count`、`tests/l5_contracts.rs::compact_cleanup_failure_after_commit_still_succeeds` | Passed |
| FC-PERSIST-POST-007 | POST | 段读取后端等价:feature `mmap` 开/关时 `source::read_whole` 与 `std::fs::read` 逐字节一致;`MmapSource::slice` 返回整段、`read_at` 越界 → `UnexpectedEof`(mmap 为优化,不改变功能语义;32 位平台长度转换失败返回 `Io`,不静默截断,解析证明登记) | `src/persist/source.rs::read_whole_matches_bytes`、`src/persist/source.rs::mmap_source_slice_and_bounds` | Passed |
| FC-PERSIST-POST-008 | POST | msec 轻量索引四区(字段字典 / zone map / bloom / 倒排)与内存结构往返一致:`flush` 全量写入;`open` 校验区结构并从磁盘倒排直接重建(经"段内槽位 → 全局槽位"重排映射),zone map 从槽位重建(等价);重开后 BM25 结果与分数逐位一致,段与未落盘增量共用同一全局统计(I21);**旧格式段(四区为空)显式跳过磁盘索引走全量重建,绝不误判 `Corrupted`**;字段字典中 `key` 保留字段唯一(同名 metadata 不注册);`ttl_map` 随 L5 落地(块级 TTL 剪枝,FC-LIFE-CPLX-001),旧格式段按次版本兼容读 | `tests/l4_contracts.rs::text_index_survives_reopen`、`tests/l4_contracts.rs::flushed_and_tail_records_share_bm25_statistics`、`tests/l4_contracts.rs::lossy_zone_intervals_survive_fail_fast_reopen`、`src/persist/recover/state.rs::empty_region_section_falls_back_to_rebuild`、`tests/l4_contracts.rs::reopen_after_update_remaps_text_index`、`src/persist/flush.rs::field_dict_keeps_single_key_field`、`src/persist/msec/index.rs::ttl_map_roundtrip_and_legacy_tail` | Passed |
| FC-PERSIST-POST-009 | POST | 文本分词口径(停用词开关)建库即锁定:新库写入 MANIFEST(三态:0=旧版未记录按开、1=关、2=开);`open` 忽略调用方冲突配置并以磁盘值为准(同时锁定查询分词),保证索引分词与查询分词同口径、绝不静默漏召回 | `tests/l4_contracts.rs::stopwords_setting_is_locked_at_creation`、`src/persist/manifest.rs::stopwords_tristate_roundtrip` | Passed |
| FC-PERSIST-POST-010 | POST | msec `delta` 区(设计 04 §2.2a)编解码往返一致:条目按 `(target, seqno)` 排序,恢复时读入并回放访问统计与关系变更;`Access` delta 仅当 `seqno` 不早于该 RowId 最新版本行时才累加(版本行的 `access` 列**始终**为写入时刻累计快照,缺失会令更旧版本的值残留后与 delta 重复累加);compaction 必须把被合并段中「其 RowId 最新版本未被新段覆盖」的 `Access` delta 携带进新段;区内条目数与 CRC 校验通过;空区视为空 `Vec`(旧段兼容) | `src/persist/msec/delta.rs::delta_roundtrip_is_sorted_and_lossless`、`src/persist/msec/delta.rs::empty_delta_is_valid`、`tests/l5_contracts.rs::delta_access_and_relations_survive_reopen`、`tests/l5_contracts.rs::access_delta_is_not_double_counted_after_later_version`、`tests/l5_contracts.rs::compaction_carries_access_delta_for_unmerged_rows`、`tests/l5_contracts.rs::compaction_keeps_access_dirty_for_history_only_row`、`tests/l5_contracts.rs::compaction_latest_in_keep_drops_covered_delta`、`tests/l5_contracts.rs::touch_then_update_single_flush_does_not_double_count` | Passed |
| FC-PERSIST-POST-011 | POST | WAL 轮转与 Checkpoint:单文件达 `CompactionPolicy.wal_file_bytes` 换新文件、单批不跨文件;打开按文件序回放,`seqno ≤ watermark` 的帧跳过(**注册表 metadata 帧同样受水位约束**;旧格式无真实 seqno 的 metadata 帧仅 `watermark == 0` 时应用),残留旧文件不得复活已注销命名空间;Checkpoint 后已完全覆盖的旧 WAL 文件可删除(删除失败仅残留,恢复时按 `seqno ≤ watermark` 跳过),未覆盖前缀绝不丢;撕裂尾按 `FC-PERSIST-INV-005` 处理 | `tests/l5_contracts.rs::wal_rotation_splits_files_without_losing_batches`、`tests/l5_contracts.rs::checkpoint_removes_covered_wal_files`、`tests/l5_contracts.rs::stale_wal_files_do_not_resurrect_unregistered_namespace`、`tests/l5_contracts.rs::namespace_registered_after_last_flush_survives_crash`、`src/persist/store/wal_writer.rs::wal_index_parsing_accepts_canonical_names_only` | Passed |
| FC-PERSIST-POST-012 | POST | **槽位归属恢复**:打开时按各段重排映射回填"槽位 → 所属段",段按编号升序回放(防御 MANIFEST 乱序);`unpersisted_slots` 只列 WAL 尾部新槽位,重开后空 flush 为空操作、compaction 不得把已落盘段当未落盘而整体丢弃 | `tests/l5_contracts.rs::reopen_preserves_segment_membership`、`src/persist/recover/state.rs::collect_versions_orders_segments_by_id` | Passed |

---

## 3. 索引与检索(index/query/score)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-INDEX-INV-005 | INV | **I5**:同一快照内 `execute()` = 候选集内暴力 + 标准融合(统计等价;向量通道由 `tests/query_contracts.rs` 与 `tests/l4_contracts.rs` 双重验收,双通道融合见 `FC-QUERY-POST-004`) | `tests/l4_contracts.rs::vector_channel_matches_bruteforce`、`tests/query_contracts.rs::brute_force_matches_reference` | Passed |
| FC-INDEX-INV-006 | INV | **I6**:过滤先行;候选集内融合后截断,绝不"先融合截断再过滤";`top_k` 小于候选数时结果集仍以过滤后候选为准(构造低重要度高排名数据证伪后过滤实现) | `tests/l4_contracts.rs::filter_is_order_independent` | Passed |
| FC-INDEX-INV-021 | INV | **I21**:BM25 统计按查询命名空间跨全部活跃段全局聚合(df/N/avgdl),只计活行,与段数无关,跨 NS 互不影响(段与未落盘增量共用同一内存倒排) | `tests/l4_contracts.rs::bm25_statistics_are_namespace_isolated`、`tests/l4_contracts.rs::flushed_and_tail_records_share_bm25_statistics`、`src/query/bm25.rs::deleted_records_are_not_counted`、`src/memory/analysis/inv.rs::accumulates_tf_and_doc_len`、`src/memory/analysis/inv.rs::namespaces_are_isolated`、`src/memory/analysis/inv.rs::empty_text_is_not_indexed` | Passed |
| FC-INDEX-PRE-001 | PRE | 建库时校验 HNSW 参数:`m ≥ 2`、`m0 ≥ m`、`ef_construction ≥ 1`、`m`/`m0 ≤ 4096`(硬上限,越限使自产 hidx 无法读回)、`ef_search ≥ 1` 且 `ef_search ≤ Limits.ef_max`(防止默认查询宽度绕过查询期上限);过滤三档阈值 `filter_post_threshold`/`filter_brute_threshold` 为 `[0,1]` 内有限值且 `brute ≤ post`;`bloom_fpp ∈ (0,1)` 且有限、`field_dict_max ≥ 1`(极小 `fpp` 会产出 `k > 64` 的不可读回段;字段上限 0 使 key bloom 缺失)。违反 → `Config`/`LimitExceeded`,绝不静默 | `tests/hnsw_contracts.rs::invalid_hnsw_params_and_thresholds_are_rejected`、`tests/l4_contracts.rs::builder_rejects_invalid_bloom_fpp` | Passed |
| FC-INDEX-POST-001 | POST | 过滤三档:①后过滤(`s > post`,全图遍历 + `ef' = max(ef,k)·min(8,1/s)`);②全图遍历 + `ef' = max(ef,k)·4` 后过滤(结果限候选,触发条件 `brute < s ≤ post` 且候选数 ≥ `max(ef,1024)`;不用约束遍历以免图被过滤切断);③候选暴力(**仅当存在过滤位图**:选择性 ≤ `brute_threshold` **或**候选数 < `max(ef,1024)`,两个触发条件各自独立成立;无过滤时不入档③(无过滤时候选即 alive、选择性恒为 1;默认 `post < 1` 走档①,合法边界 `post = 1.0` 走档②,二者同为全图遍历、不改变语义));档③恒等于「候选位图内暴力」,档①②与之统计等价(口径:`ef'·s ≳ 4k`,`ef→∞` 精确;设计 05 §8) | `src/index/filtered.rs::tier_selection_matches_selectivity_and_candidate_cap`、`tests/hnsw_contracts.rs::filter_tier_three_matches_candidate_bruteforce`、`tests/hnsw_contracts.rs::filter_brute_trigger_conditions_are_independent`、`tests/hnsw_contracts.rs::filter_post_tier_matches_candidate_bruteforce`、`tests/hnsw_contracts.rs::filter_amplified_tier_matches_candidate_bruteforce` | Passed |
| FC-INDEX-POST-002 | POST | **I5 收敛**:`ef → ∞` 时 HNSW 结果收敛于精确暴力 | `tests/hnsw_contracts.rs::ann_converges_to_bruteforce_with_large_ef` | Passed |
| FC-INDEX-POST-003 | POST | **排序全等性**:同一快照内任意两次 `execute()`(同参数)结果完全一致(同分按 RowId 升序) | `tests/query_contracts.rs::search_order_is_total_and_stable` | Passed |
| FC-INDEX-POST-005 | POST | ANN 结果 ⊆ alive ∩ 过滤位图,**且 alive 按目标命名空间与 TTL 判定**(其他 NS 与逻辑过期记录不得入选);死节点与未被 alive 选中的历史版本只可穿越、不可入选(设计 05 §7/§12);`as_of` 历史视图按历史 alive 位图返回已删记录、当前视图不返回,且 `snapshot_at` 保留索引句柄(不得静默降级暴力) | `tests/hnsw_contracts.rs::ann_excludes_deleted_records`、`tests/hnsw_contracts.rs::ann_respects_namespace_and_ttl_visibility`、`tests/hnsw_contracts.rs::ann_after_as_of_matches_bruteforce`、`src/memory/temporal.rs::snapshot_at_preserves_index_handle`、`src/index/hnsw.rs::search_results_respect_alive_bitmap` | Passed |
| FC-INDEX-POST-007 | POST | hidx(HID1)编解码往返恢复同一图(节点数/层级/邻接/入口/参数);`decode(encode(g))` 与 `g` 一致 | `src/index/hidx.rs::hidx_roundtrip_restores_graph` | Passed |
| FC-INDEX-POST-008 | POST | 持久库 `flush` 写 `hidx` 并在 MANIFEST 登记 `hidx_crc`/`entry_slot`/`entry_level`;重开经重排映射从 hidx 载入索引(`stats().segments[*].index_nodes > 0`)且检索正确;重排映射在非恒等场景(删除/多版本导致段内槽位次序与全局槽位次序不同)亦正确(小 `ef` 多查询召回 + `ef→∞` 精确双重验收) | `tests/hnsw_contracts.rs::reopen_loads_hnsw_from_hidx`、`tests/hnsw_contracts.rs::reopen_after_delete_remaps_slots` | Passed |
| FC-INDEX-POST-009 | POST | ANN Recall@10 ≥ 0.95(`ef=128`,`HnswParams::default()`,段行数超过 `brute_force_max_rows` 时走图);**分派为严格「超过」:行数 ≤ `brute_force_max_rows` 时恒暴力,与索引是否存在无关**;两种固定种子分布(随机均匀与 8 簇合成数据)分别达标 | `tests/hnsw_contracts.rs::ann_recall_at_ten_meets_threshold`、`src/memory/search.rs::search_dispatches_to_index_when_prefix_exceeds_brute_threshold`、`src/memory/search.rs::search_bruteforces_when_prefix_does_not_exceed_threshold` | Passed |
| FC-INDEX-INV-007 | INV | 图节点 id ∈ [0,count);每层度数 ≤ M0(第 0 层)/ M(上层);无自环、邻居 id 有效;入口节点层级 = 全图最高层。构建与 hidx 载入两条路径恒成立(载入解码即校验,违反 → `Corrupted`) | `src/index/hnsw.rs::graph_degree_and_self_loop_invariants`、`src/index/hidx.rs::hidx_rejects_degree_above_layer_bound`、`src/index/hidx.rs::hidx_rejects_entry_level_below_max` | Passed |
| FC-INDEX-INV-008 | INV | 查询 = 各段索引 ANN + 未落盘尾部暴力,`TopK` 归并;`ef→∞` 时结果 ≡ 全量候选暴力(设计 05 §9;多段形态下每段独立分派过滤三档) | `tests/hnsw_contracts.rs::ann_merges_prefix_with_unflushed_tail`、`tests/l5_contracts.rs::multi_segment_search_matches_bruteforce` | Passed |
| FC-INDEX-ERR-001 | ERR | hidx 魔数不符/负载 CRC 翻转 → `Corrupted`;主版本过新 → `UnsupportedVersion`(I18);头部 `ef_construction = 0`/入口层级低于最高层/逐层度数越界/截断/头 CRC/布局不符 → `Corrupted`;**载入节点数与恢复槽位数不一致 → `Corrupted`**;任意输入不 panic、不静默(proptest「接受即往返」+ 定向用例) | `src/index/hidx.rs::hidx_rejects_bad_magic`、`src/index/hidx.rs::hidx_detects_payload_corruption`、`src/index/hidx.rs::hidx_rejects_higher_major`、`src/index/hidx.rs::hidx_rejects_zero_ef_construction`、`src/index/hidx.rs::hidx_rejects_entry_level_below_max`、`src/index/hidx.rs::hidx_rejects_degree_above_layer_bound`、`src/index/hidx.rs::hidx_rejects_truncated_or_malformed_header`、`src/index/hidx.rs::hidx_rejects_bad_layout`、`src/index/hidx.rs::hidx_rejects_bad_neighbors`、`src/index/hidx.rs::hidx_decode_never_panics_on_arbitrary_bytes`、`src/index/hnsw.rs::load_rejects_node_count_mismatch` | Passed |
| FC-INDEX-ERR-002 | ERR | MANIFEST 引用的 `hidx` 缺失/整文件 CRC 不符:fail-fast 打开 → `Corrupted`;可写非 fail-fast 打开 → 降级暴力(`stats().segments[*].index_nodes == 0`)、库仍可读,`db.check()` 报告该段损坏 | `tests/persist_contracts.rs::missing_hidx_degrades_or_rejects`、`tests/persist_contracts.rs::corrupt_hidx_degrades_or_rejects` | Passed |
| FC-INDEX-ERR-003 | ERR | `hidx::encode` 编码期防御:图节点数/节点表/邻接区字节数超 `u32` → `LimitExceeded`(`field` 标明具体字段);单层度数超 `u16` → `Inconsistent`(违反 `FC-INDEX-INV-007` 度数上界)。绝不静默截断(`as u32`/`as u16`)。`Builder` 校验(度数 ≤ 4096)下正常构建不可达;长度分支需 >4 GiB 邻接区,64 位平台以解析证明登记,度数分支以定向测试证伪 | `src/index/hidx.rs::hidx_encode_rejects_degree_above_u16` | Passed |
| FC-SCORE-INV-027 | INV | **I27**:同一 `(rowid, query_id)` 的反馈至多计一次;对不可见记录(不存在/已墓碑/已过期)的反馈返回 `false` 且**不占用幂等键**(后续该 `RowId` 重新可见时首次反馈仍生效);`execute()` 缺省生成的 `QueryId` 由进程级全局分配器分配(跨库实例共享同一编号空间,保证不冲突),调用方显式指定时须自行保证唯一性 | `tests/query_contracts.rs::feedback_is_idempotent_per_query` | Passed |
| FC-SCORE-POST-001 | POST | `Scoring::default()` 与未开启 `score()` 的排序全等 | `tests/query_contracts.rs::default_scoring_matches_similarity_order` | Passed |
| FC-SCORE-POST-002 | POST | `Scoring::floor` 下的候选满足 `ŝ ≥ floor` 或 `S = 0`:归一化相似度 `ŝ < floor` 时综合分清零,`ŝ = floor` 为保留边界(实现用严格小于,等于保留);`floor = 0` 时恒不清零 | `tests/query_contracts.rs::scoring_floor_zeroes_below_threshold` | Passed |
| FC-SCORE-POST-003 | POST | 放大 ef 后综合排序相对召回损失 ≤ 2% | 待补 | Planned |
| FC-QUERY-ERR-001 | ERR | DSL 任意输入不 panic,返回结构化 `FilterParse`(I7);解析器设嵌套深度上限,错误携带字节位置;数字非有限(如 `1e999`)与相对时间量超出 `f64` 精确范围/ISO 8601 可往返范围(4 位年份) → `FilterParse`,绝不整数溢出 panic,也绝不产出 `Display` 读不回的值 | `tests/l4_contracts.rs::dsl_never_panics_on_arbitrary_input`、`src/query/parse/mod.rs::malformed_inputs_report_position`、`src/query/parse/mod.rs::out_of_range_numbers_and_durations_are_rejected`、`src/query/parse/mod.rs::deeply_nested_input_is_rejected_without_panic` | Passed |
| FC-QUERY-ERR-002 | ERR | `Not` 对缺失字段采用三值语义(缺失 → `Not` 亦为 false) | `tests/query_contracts.rs::filter_uses_kleene_three_valued_logic` | Passed |
| FC-QUERY-POST-001 | POST | 谓词类型规则:数值比较 `Int`/`Num` 互通;`Ts` 仅与 `Ts` 比较;`Contains`/`StartsWith`/`EndsWith`/`Glob` 要求字符串或数组;类型不匹配求值为 `Unknown`(不命中) | `tests/query_contracts.rs::predicate_type_rules` | Passed |
| FC-QUERY-POST-002 | POST | **DSL 往返**:解析 → `Display` → 再解析等价;解析 → `to_meta` → `from_meta` 等价;`always`/`never` 常量闭合,程序构造的空 `And`/`Or`/`In` 规约为等价真值常量(设计 06 §1) | `tests/l4_contracts.rs::dsl_display_and_json_roundtrip`、`src/query/display.rs::empty_lists_print_as_truth_constants`、`src/query/json.rs::json_encodes_empty_lists_as_truth_constants` | Passed |
| FC-QUERY-POST-003 | POST | **BM25 打分**(设计 06 §3.2):`k1=1.2`、`b=0.75`;IDF 稀有词得分更高、TF 饱和(有界 `k1+1`)、长度归一(同 tf 短文档更优);精确分数与手算公式一致 | `tests/l4_contracts.rs::bm25_formula_behaviour`、`tests/l4_contracts.rs::bm25_exact_scores_match_formula`、`src/query/bm25.rs::rare_term_ranks_above_common_term`、`src/query/bm25.rs::term_frequency_saturates`、`src/query/bm25.rs::shorter_document_wins_at_equal_tf` | Passed |
| FC-QUERY-POST-004 | POST | **双通道融合**(设计 06 §4):默认 `Rrf{k:60}`(只比名次);`Weighted` 在本次结果集内 min-max 归一(Euclidean 距离先取负;零极差通道(单点或全同分)归一值取 1,极小非零极差仍按公式缩放,极端极差不产生 NaN);同分按 `RowId` 升序;`Fusion` 未同时启用双通道或 `Weighted.alpha` ∈ `[0,1]` 外/非有限 → `Config` | `tests/l4_contracts.rs::hybrid_fusion_and_validation`、`src/query/fusion.rs::rrf_matches_design_example`、`src/query/fusion.rs::weighted_flips_distance_channel`、`src/query/fusion.rs::weighted_single_result_channel_normalizes_to_one`、`src/query/fusion.rs::weighted_tiny_span_still_scales`、`src/query/fusion.rs::weighted_extreme_span_stays_finite`、`src/query/fusion.rs::ties_break_by_rowid` | Passed |
| FC-QUERY-POST-005 | POST | **计划器等价性**(设计 06 §2):zone map/bloom 块位图只剪"必然不命中"的块;`Not` 与无摘要条件保持全 1;类型不匹配、`f64` 无法精确表示的整数(绝对值 > 2^53)一律返回全 1;**与保留字段同名的 metadata 一律不进 zone map**(`rowid`/`key`/`__ns`/`access_count`/`last_access` 及 `created_at` 等系统字段的行级值优先于同名 metadata,据 metadata 统计剪枝会漏报),但保留名对象下的**子路径**(如 `key.x`)按 `meta::get_path` 语义照常观察;非数值字段不参与区间统计(只保证不漏报),`exists` 以"字段是否出现(含非数值/null)"判定;最终候选与逐行三值求值全等 | `tests/l4_contracts.rs::plan_filter_matches_pointwise_count`、`tests/l4_contracts.rs::planner_never_prunes_possible_blocks`、`src/query/plan.rs::plan_candidates_match_bruteforce`、`src/query/plan.rs::block_pruning_skips_impossible_blocks`、`src/memory/analysis/zones.rs::kind_conflict_disables_block_pruning`、`src/memory/analysis/zones.rs::reserved_metadata_is_shadowed_and_not_indexed`、`src/memory/analysis/zones.rs::reserved_object_subpaths_are_still_indexed`、`src/query/plan.rs::reserved_metadata_shadowing_never_prunes`、`src/query/plan.rs::dotted_reserved_subpath_never_prunes`、`src/memory/pred_eval.rs::reserved_names_resolve_to_reserved_values`、`src/query/plan.rs::key_bloom_rejects_absent_key`、`src/query/plan.rs::key_bloom_skips_row_evaluation` | Passed |
| FC-QUERY-POST-006 | POST | **历史视图 TTL**(设计 07):`as_of(t)` 检索的 TTL 可见性以视图时刻 `t` 为准(`expires_at > t`),与墙上时钟无关;`SnapshotHandle` 路径同样以快照时刻判定 | `tests/l4_contracts.rs::historical_search_uses_view_time_for_ttl` | Passed |
| FC-QUERY-POST-007 | POST | ISO 8601 ↔ Unix 毫秒(`query::iso`):4 位年份、时间部分(含 `T`)整体可缺省,`T` 后 `hh:mm` 必填且时/分/秒各**恰好 2 位**、秒与小数秒可缺省;`T`/`Z` 大小写兼容,时区 `±hh:mm`/`±hhmm`(时分各恰好 2 位);小数秒最多 9 位、截断到毫秒且不足 3 位按十分位/百分位补零(绝不读入后续时区字符);秒域 0–59(不支持闰秒 60);日历越界(2 月 30 日等)拒绝而非归一;`[MIN_ROUNDTRIP_MS, MAX_ROUNDTRIP_MS]` 内 `parse(format(ms)) == ms` | `src/query/iso.rs::parses_with_millis_and_offset`、`src/query/iso.rs::date_only_defaults_to_midnight_utc`、`src/query/iso.rs::accepts_lowercase_and_compact_offset`、`src/query/iso.rs::rejects_calendar_and_format_violations`、`src/query/iso.rs::pre_epoch_and_roundtrip`、`src/query/iso.rs::format_parses_back_for_sample_range`、`src/query/iso.rs::roundtrip_covers_range_samples`、`src/query/iso.rs::roundtrip_bounds_cover_exact_range`、`src/query/iso.rs::fractional_seconds_pad_to_millis` | Passed |
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
| FC-MODEL-POST-004 | POST | **版本链保留**:每个 RowId 的最新版本(或最新墓碑,以维持当前可见状态)与 `tx_ms ≥ now − history_horizon` 的历史版本被 compaction 保留;仅 `tx_ms < now − horizon` 的历史版本可回收;**整链回收以最新版本为准**:最新版本为窗口外的墓碑/逻辑过期时整链(无论各历史版本 `tx_ms`)一并回收,绝不只回收 `latest` 而留下悬挂/旧活版本复活(时钟回拨下同样成立);整链逻辑过期同理;`as_of` 在窗口内不随后续写入/compaction 变化,`horizon = None`(默认)时永久保留且死比率不触发重写 | `tests/l5_contracts.rs::history_horizon_reclaims_old_versions`、`tests/l5_contracts.rs::compaction_reclaims_tombstones_under_horizon`、`tests/l5_contracts.rs::compaction_preserves_snapshot_and_as_of_within_horizon`、`tests/l5_contracts.rs::compaction_never_revives_deleted_record_under_clock_skew`、`src/life/compact.rs::reclaims_whole_chain_when_latest_tombstone_outside_window`、`src/life/compact.rs::keeps_latest_tombstone_inside_window_and_reclaims_history` | Passed |
| FC-MODEL-POST-005 | POST | `predecessors(to, kinds)` 只返回 `edge.to == to` 且 `edge.kind ∈ kinds`、两端存活的边;`RelationIndex::Outgoing` 与 `Both` 结果一致(反向索引只加速、不改语义) | `tests/model_contracts.rs::predecessors_returns_incoming_edges`、`tests/l5_contracts.rs::relation_index_modes_agree_after_reopen` | Passed |
| FC-MODEL-POST-006 | POST | `consolidate(policy)` 以 `threshold` 为聚类相似度下界:相似度 ≥ threshold 的近似重复聚为一簇,簇成员 ≥ 2 才合并;`keep_sources=true` 不删来源;策略参数非法(`threshold` 非有限值或越界 [0,1]、`max_cluster = 0`)→ `Config`,绝不静默空转或索引越界 | `tests/model_contracts.rs::consolidate_merges_cluster_and_keeps_sources`、`tests/model_contracts.rs::consolidate_rejects_invalid_policy` | Passed |
| FC-MODEL-POST-007 | POST | `RelationIndex::Both` 时段内 `relations` 区追加按 `(to, kind, from)` 排序的反向表;`edges::parse` 返回正向/反向两表;关系区带**全量/增量标志**(次版本 4 起):带 `FLAG_FULL` 的段(compaction/首段)恢复时先重置关系表再应用,其余段(含全部旧格式段)仅 upsert——旧库恒为单段(重置与 upsert 等价),旧格式增量段按 upsert 才不会清掉先前段的边;compaction 新段为全量快照,被删除的旧边绝不因并集复活;恢复后入边集合与 `Outgoing` 模式逐边一致、重开不丢边;默认 `Outgoing` 不写反向表 | `src/persist/edges.rs::edges_roundtrip_with_reverse`、`tests/l5_contracts.rs::reverse_relation_table_survives_reopen`、`tests/l5_contracts.rs::compaction_does_not_resurrect_removed_edges`、`src/persist/recover/state.rs::relations_without_full_flag_are_upserted` | Passed |
| FC-MODEL-STA-001 | STA | 记忆版本五元组 `M=(S={Active,Shadowed,Reclaimed}, E={Update,Upsert,Delete,AsOf,Compact}, δ: Active×{Update,Upsert,Delete}→Shadowed(旧)∧Active(新), Shadowed×AsOf→Shadowed(历史可见), Shadowed×Compact→Reclaimed, s0=Active, F={Reclaimed})`;非法转移:当前读路径(`get`/`search`/`iter`/`count`)命中 `Shadowed` 必须不可见并显式拦截,绝不静默返回 | `tests/model_contracts.rs::model_version_lifecycle_states` | Passed |

---

## 5. 生命周期(life)

| 编号 | 类型 | 形式化规范 | 对应测试 | 状态 |
|---|---|---|---|---|
| FC-LIFE-PRE-001 | PRE | 建库校验 `CompactionPolicy`:`tier_ratio`/`tier_count` ≥ 2、`segment_rows` ≥ 1、`dead_ratio`/`io_budget` 为 `[0,1]` 内有限值;违反 → `Config`(避免触发条件永假或除零) | `tests/l5_contracts.rs::builder_rejects_invalid_compaction_policy` | Passed |
| FC-LIFE-INV-008 | INV | **I8**:活跃段数 ≤ `(T−1)·log_r(N/B)+c`(size-tiered:段按 `row_count` 分层,同层段数 < `tier_count` 否则触发合并;死比率超线时重写该段,**死比率只计窗口外可回收死行**,`horizon = None` 时无回收收益、不触发重写);WAL 总量 ≤ `wal_bytes`(WAL 上界部分已随 L2 落地于 `FC-PERSIST-INV-004`) | `tests/l5_contracts.rs::compaction_bounds_segment_count`、`tests/l5_contracts.rs::auto_compaction_triggers_in_background`、`tests/l5_contracts.rs::compact_without_horizon_does_not_rewrite_dead_segments`、`src/life/compact.rs::dead_ratio_counts_only_reclaimable_versions` | Passed |
| FC-LIFE-INV-009 | INV | **I9**:逻辑过期/墓碑记录在常规读路径永不返回(仅 `iter_with(..., true)` 审计入口可见);内部辅助路径(dedup 判重、`stats` 计数、`consolidate` 候选、`forget` 目标)同样排除逻辑过期记录;物理回收仅在 compaction 提交后 | `tests/memory_contracts.rs::delete_hides_records_from_reads`、`tests/memory_contracts.rs::logically_expired_hidden_from_internal_paths` | Passed |
| FC-LIFE-INV-010 | INV | **I10**:compaction 任意步骤崩溃 → 恢复后数据集 = 提交前状态;新段为孤儿(下次启动清理),旧 MANIFEST 完好、无损回滚;提交以「写 `MANIFEST.<v>` → 原子换 `current`」完成 | `tests/l5_contracts.rs::compaction_failure_keeps_state_and_returns_idle` | Passed |
| FC-LIFE-INV-011 | INV | **I11**:备份目录独立 `open` + `check` 通过(**FC-PERSIST-POST-004** 已覆盖复制语义) | `tests/persist_contracts.rs::backup_is_independently_openable` | Passed |
| FC-LIFE-INV-017 | INV | **I17**:`SnapshotHandle` 存活期间看到固定 `ReaderView` 的完整视图(段集 + 取快照时的可变表快照);后台 compaction 提交不改变其可见性与正确性(旧视图经 `Arc` 保持,旧段延迟回收)。注:`SnapshotHandle` 视图一致部分已随 L1 落地(`tests/memory_contracts.rs::seqno_and_rowid_stable_prop` 观测水位快照) | `tests/l5_contracts.rs::compaction_preserves_snapshot_and_as_of_within_horizon` | Passed |
| FC-LIFE-INV-023 | INV | **I23**:自动遗忘默认关闭;删除可审计(墓碑在 `history_horizon` 内保留,默认永久,经 `iter_with(..., true)` 可见),绝不静默 | `tests/life_contracts.rs::retain_forgets_below_threshold`、`tests/l5_contracts.rs::auto_retention_is_off_by_default`、`tests/l5_contracts.rs::auto_retention_forgets_expired_records` | Passed |
| FC-LIFE-POST-001 | POST | `retain` 返回 `forgotten` 与 `sampled_ids` 与实际墓碑一致 | `tests/life_contracts.rs::retain_forgets_below_threshold` | Passed |
| FC-LIFE-POST-002 | POST | 保留分公式 `score = importance·2^(−age/T½) + w·ln(1+access_count)`;`age = max(0, now − max(valid_from, last_access))`(`valid_from` 取记录有效时间起,`last_access` 取最近访问;`T½=0` 时衰减项为 0,`age<0` 按 0);`min_importance`/`access_weight` 任一含非有限值 → `Config`(绝不静默永不遗忘) | `src/memory/lifecycle.rs::retention_score_formula`、`tests/life_contracts.rs::error_taxonomy_is_specific` | Passed |
| FC-LIFE-POST-003 | POST | **增量段 flush**:`flush` 把 `seqno > watermark` 的槽位与自上次 flush 的访问/关系 `delta` 物化进**新段**;旧段保持活跃、不改写、不入 trash;`watermark` 推进、WAL Checkpoint;读取面跨段合并后与全量内存状态逐位一致;无新增且无 delta 时为空操作(不产段) | `tests/l5_contracts.rs::incremental_flush_appends_segments`、`tests/l5_contracts.rs::incremental_flush_does_not_rewrite_committed_segments`、`tests/l5_contracts.rs::empty_flush_is_noop` | Passed |
| FC-LIFE-POST-004 | POST | **访问统计攒批**:查询命中把 `RowId` 追加进内存缓冲(同 RowId 按键累加;缓冲条目数有上限,超限后新 RowId 丢弃,防维护线程停止时无界增长),攒批(默认 `access_flush_interval` = 30s)把缓冲合并为每 RowId 一条 WAL `TouchRow`(`access_delta` = 缓冲累计值 ≥ 1);崩溃最多丢一个攒批周期的访问计数,只影响遗忘速度估计、不影响记录可见性与检索正确性;显式 `touch` 仍即时 WAL 落盘 | `tests/l5_contracts.rs::access_hits_are_batched_and_flushed` | Passed |
| FC-LIFE-POST-005 | POST | **命名空间路径规范化**:`namespace(path)` 去首尾 `/`、合并连续 `/`,规范化后为空 = 根命名空间;键唯一性按规范化路径;深度 > `Limits.ns_depth` 或非法字符于**首次写入**时 → `Config`;`list_namespaces` 按规范化路径字典序返回 | `tests/l5_contracts.rs::namespace_paths_are_normalized_and_boundary_matched`、`tests/l5_contracts.rs::namespace_depth_limit_reported_at_first_write` | Passed |
| FC-LIFE-POST-006 | POST | **命名空间注销持久化**:`drop_namespace(path)` 按 `/` 段边界匹配(`a/b` 不含 `a/bc`),墓碑命中记录并移除注册表,返回墓碑**行数**;注销经 WAL 帧持久化,崩溃恢复后已注销路径不再出现;`NsId` 水位不回退、`NsId` 永不复用 | `tests/l5_contracts.rs::drop_namespace_is_durable_across_crash`、`tests/l5_contracts.rs::namespace_paths_are_normalized_and_boundary_matched` | Passed |
| FC-LIFE-POST-007 | POST | `SnapshotHandle::stats()` 返回 `SnapshotStats { version, segments, rows }`:段数/行数取快照钉住视图、`version` 为视图基线序号水位;快照统计不随后续写入/compaction 变化;`SnapshotNamespace::get_many_by_rowid` 与 `Namespace` 读取面一致 | `tests/l5_contracts.rs::snapshot_stats_pin_view` | Passed |
| FC-LIFE-POST-008 | POST | `backup_to` 同盘优先硬链接、失败/跨盘回退逐文件复制;`BackupReport.hardlinked` 如实报告;两条路径产物均满足 `FC-PERSIST-POST-004` 可独立 `open` + `check` | `tests/l5_contracts.rs::backup_hardlinks_segments_when_possible`、`src/persist/store/snapshot.rs::hardlink_failure_falls_back_to_copy` | Passed |
| FC-LIFE-POST-009 | POST | `check()` 报告每段墓碑/逻辑过期占比与建议动作(如「建议合并 N 个段」);占比统计只读元数据(不读向量),不把健康库判为损坏 | `tests/l5_contracts.rs::stats_report_latency_dead_ratio_and_fsck_suggestions` | Passed |
| FC-LIFE-STA-001 | STA | compaction 五元组 `M=(S={Idle,Running,Paused}, E={Trigger,Step,Pause,Resume,Abort,Done,Fail}, δ(Idle,Trigger)=Running, δ(Running,Pause)=Paused, δ(Paused,Resume)=Running, δ(Running,Done)=Idle, δ(Running,Fail)=Idle, δ(Paused,Abort)=Idle, s0=Idle, F={Idle})`;`stats().compaction` 反映当前态(`Paused` 保留进度与段列表);`pause()` 只在「新段写完、MANIFEST 提交前」这一个步骤边界检查生效,提交后不可中止;非法转移(如 `Idle` 上 `Resume`/`Pause`)不改变状态 | `src/memory/ops.rs::compaction_state_transitions_follow_spec`、`tests/l5_contracts.rs::compaction_respects_pause`、`tests/l5_contracts.rs::pause_during_running_aborts_before_commit`、`tests/l5_contracts.rs::compaction_bounds_segment_count` | Passed |
| FC-LIFE-ERR-001 | ERR | compaction 运行期失败(ENOSPC/I/O/编码错误)→ 以 `Io`/`Corrupted` 上报并回到 `Idle`,已提交 MANIFEST 与数据不变;孤儿新段由下次启动清理;MANIFEST 提交点之后的旧段清理失败**不视为运行期失败**(内存视图先与新 MANIFEST 对齐,旧段成为孤儿、由下次启动清理),绝不静默吞错或半提交 | `tests/l5_contracts.rs::compaction_failure_keeps_state_and_returns_idle`、`tests/l5_contracts.rs::compact_cleanup_failure_after_commit_still_succeeds` | Passed |

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
| FC-GLOBAL-ERR-001 | ERR | 库绝不 panic;文档化例外共三类:① `filter!` 字面量(`src/lib.rs`);② async `spawn_blocking`(async 门面落地后启用);③ 槽位下标 `u32::try_from(..).expect` 六处(`src/memory/search.rs` 的 `scan_chunk` 候选收集、`src/memory/table/state.rs` 的 `rebuild_indexes`/`install_segment`/`prune_reclaimed`、`src/persist/flush.rs` 的 `build_index`、`src/query/plan.rs` 的 `compile`),均由 `FC-MEM-INV-004`(槽位下标 ≤ `u32::MAX`)保证不可达;另有 `src/persist/wal/codec.rs` 的 `encode_frame` 负载长度转换(单帧负载远小于 `u32::MAX`,`FC-GLOBAL-PRE-003` 限额)一并登记为文档化例外 | `tests/life_contracts.rs::l1_api_smoke_never_panics` | Passed |
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
| FC-PERSIST-CPLX-004 | CPLX | zone map:**写路径增量维护**(每条记录每字段均摊 $O(1)$、共 $O(N\cdot\text{fields})$,随快照以 `Arc` COW 共享;存在长命快照句柄时单次写可能深拷贝索引,属已登记取舍),flush 仅编码 $O(\text{blocks}\cdot\text{fields})$;查询期块级剪枝 $O(\lceil n/1024\rceil \times \text{predicates})$;落盘空间 17 B/块/字段(`has_any`/`mixed` 标志仅存内存,重开时 zone map 从槽位重建)。L4 起实际写入 msec `zmap` 区并服务于查询计划器 | 解析证明(设计 04 §5.2)+ 哨兵 `tests/l4_contracts.rs::plan_filter_matches_pointwise_count` | Passed |
| FC-PERSIST-CPLX-005 | CPLX | bloom:构建 $O(N_{key}\cdot k)$、判定 $O(k)=O(7)$;空间 $1.44\log_2(1/p)$ bit/元素(**承诺在元素数 ≤ 初始容量 65536 时成立**;写路径超出后位图饱和,只升误报率、绝不漏报)。极小/非法 `fpp` 在校验与构造两层夹紧,`k` 恒落在可落盘范围 `[1,64]`。L4 起实际写入 msec `bloom` 区,供 `key` 等值预筛 | 解析证明(设计 04 §5.3 双哈希)+ 哨兵 `tests/l4_contracts.rs::text_index_survives_reopen`、`src/memory/analysis/bloom.rs::extreme_fpp_stays_within_storable_k` | Passed |
| FC-PERSIST-CPLX-006 | CPLX | MANIFEST 提交:时间 $O(S_{\text{seg}})$ 写新文件;空间保留 2 版 | 解析证明(设计 04 §6;`manifest::encode` 逐段线性)+ 哨兵 `src/persist/manifest.rs::manifest_roundtrip` | Passed |
| FC-PERSIST-CPLX-007 | CPLX | `open` 恢复:时间 $O(\text{段总字节} + \text{WAL 字节})$(逐段读入并校验 + 回放);空间 $O(\text{段总字节} + \text{WAL 字节})$——L2 将段/WAL 整读入内存,mmap 读路径优化已随 L3 落地,真正惰性访问待 L5/L6 段句柄重构(设计 04 §11) | 哨兵 `tests/persist_contracts.rs::reopen_after_drop_recovers_from_wal`;解析证明(设计 04 §7;`read_segment_bytes` 逐段读入) | Passed |
| FC-PERSIST-CPLX-008 | CPLX | 单点写 `insert`:时间 $O(1)$ 内存 + WAL 追加(fsync 按 `FsyncPolicy`);空间 $O(d)$ | 哨兵 `tests/persist_contracts.rs::reopen_after_close_recovers_records`;解析证明(HashMap 追加 + 定长帧) | Passed |
| FC-PERSIST-CPLX-009 | CPLX | 单点读 `get(key)`:时间 $O(\log n)$ + 一次记录读(实现为 HashMap 期望 $O(1)$ $\subseteq O(\log n)$);`get_by_rowid`: $O(\log n)$ 版本链定位 | 哨兵 `tests/persist_contracts.rs::namespace_and_rowid_survive_reopen`;解析证明(设计 04 §5.5) | Passed |
| FC-PERSIST-CPLX-010 | CPLX | `as_of(t)`:时间 $O(V + N_c\cdot d)$($V$ = 全部物理版本数,`temporal::snapshot_at` 单遍择版本;当前实现不随段数$S_{\text{seg}}$分层加速);空间 $O(V + N_c)$(随历史窗口与候选规模增长) | 哨兵 `tests/persist_contracts.rs::version_chain_survives_reopen`;解析证明(设计 04 §5.5) | Passed |
| FC-PERSIST-CPLX-011 | CPLX | 增量 flush:时间 $O(\Delta + D)$($\Delta$ = 未物化槽位数,$D$ = 自上次 flush 的访问/关系变更数),不重写已提交段;空间额外 $O(\Delta + D)$ | 解析证明(`Store::flush` 位于 `src/persist/store/snapshot.rs`,仅编码 `unpersisted_slots()` 与 `build_delta`,不读旧段)+ 哨兵 `tests/l5_contracts.rs::incremental_flush_does_not_rewrite_committed_segments`、`tests/l5_contracts.rs::empty_flush_is_noop` | Passed |

#### 9.2.4 L3 索引层(index)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-INDEX-CPLX-001 | CPLX | HNSW 单点插入:时间 $O(d\cdot(ef_c\cdot M_0 + M\log_M N))$;构建 $O(N\cdot d\cdot ef_c\cdot M_0)$;空间 $\approx(8M+20)$ B/节点 + 边表。注:$M$/$M_0$ 为**有界常数**(≤4096,FC-INDEX-PRE-001);启发式选邻与修剪的 $M_0$ 多项式项(变量展开时最坏额外 $O(d\cdot M_0^3\cdot\log_M N)$)在最坏口径下含于系数,不影响 $N$/$d$/$\log N$ 的渐进结论 | 操作计数单测 `src/index/hnsw.rs::build_distance_calls_scale_linearly` | Passed |
| FC-INDEX-CPLX-002 | CPLX | HNSW 查询:期望上界 $O(d\cdot ef\cdot M_0)$(实测 $\approx(2\text{–}5)\cdot ef$ 次点积);空间期望 $O(ef\cdot M_0)$(`visited` 集合覆盖已展开节点的邻边)。注:本条目只约束索引内部;调用方 `memory::search` 当前的候选收集与 alive/过滤位图构造仍为 $O(N)$(L4 已引入 zone map/bloom 块级下推减少行级求值,但候选遍历本身仍为 $O(N)$,待 L5/L6 段句柄重构消除,设计 05 §8) | 操作计数单测 `src/index/hnsw.rs::search_distance_calls_bounded_by_ef` | Passed |
| FC-INDEX-CPLX-003 | CPLX | 上层下降:时间期望 $O(d\cdot M\cdot\log_M N)$(依赖层级随机分布,§9.1 口径③);层高期望 $O(\log_M N)$ | 操作计数单测 `src/index/hnsw.rs::level_height_grows_logarithmically` | Passed |
| FC-INDEX-CPLX-004 | CPLX | hidx 编解码:时间 $O(\text{nodes}+\text{edges})$、空间 $O(\text{bytes})$ | 操作计数单测 `src/index/hidx.rs::hidx_encode_decode_scale_linearly` | Passed |

#### 9.2.5 L4 检索与排序层(query/score)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-QUERY-CPLX-001 | CPLX | DSL 解析:时间 $O(L)$ 单遍;空间 $O(\|\phi\|)$ | 解析证明(设计 06 §1.2:单遍递归下降)+ 哨兵 `tests/l4_contracts.rs::dsl_parse_handles_large_input_once`(2000 项 Or 链) | Passed |
| FC-QUERY-CPLX-002 | CPLX | 计划编译:时间 $O(N + \text{blocks}\times\text{predicates})$、空间 $O(n/8)$ 块位图 + $O(N_c)$ 候选。注:$N$ 为视图物理槽位数(逐行判定命名空间/可见性);块掩码求值本身为 $O(\text{blocks}\times\text{predicates})$。内存架构下无段句柄直读,该 $O(N)$ 项待 L5/L6 段句柄重构消除 | 解析证明(设计 06 §2)+ 哨兵 `tests/l4_contracts.rs::plan_filter_matches_pointwise_count` | Passed |
| FC-QUERY-CPLX-003 | CPLX | BM25 打分:时间 $O(N_{ns} + 2\sum_{t\in Q} df_t)$ postings 访问;空间 $O(\min(N_{ns}, \sum_{t\in Q} df_t))$(第二遍物化全部命中文档的分数映射,TopK 另计 $O(k)$)($N_{ns}$ = 查询命名空间有文本的物理槽位数,用于可见性过滤后的 N/avgdl 统计;段统计不可变后时间项可降为 $O(S_{\text{seg}})$,待 L5/L6) | 解析证明(设计 06 §3.2 两遍法)+ 哨兵 `tests/l4_contracts.rs::bm25_formula_behaviour` | Passed |
| FC-QUERY-CPLX-004 | CPLX | RRF/加权融合:时间 $O(k)$、空间 $O(k)$ | 解析证明(设计 06 §4:只对两通道 top-k 名次表操作)+ 哨兵 `tests/l4_contracts.rs::hybrid_fusion_and_validation` | Passed |
| FC-SCORE-CPLX-001 | CPLX | 综合重排/归一化:时间 $O(m)$($m$ = 候选数),每候选 $O(1)$;空间 $O(m)$ | 待补 | Planned |
| FC-SCORE-CPLX-002 | CPLX | 联想扩展:时间 $O(\text{seeds}\cdot\text{max\_nodes}\cdot\text{avg\_degree})$(有界 BFS,$hops\le 3$);空间 $O(\text{max\_nodes})$ | 待补 | Planned |
| FC-SCORE-CPLX-003 | CPLX | MMR 贪心:时间 $O(k^2)$(冗余相似度缓存后;现算为 $O(k^2\cdot d)$);空间 $O(k)$ | 待补 | Planned |

#### 9.2.6 L5 生命周期层(life)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-LIFE-CPLX-001 | CPLX | TTL 逻辑过期:时间 $O(\text{blocks})$(块级 `min(expires_at)` 剪枝,整块全未过期时不逐行判定);空间 8 B/块(`ttl_map` 落于 msec zmap 区尾,见 FC-PERSIST-POST-008) | `src/query/plan.rs::ttl_unexpired_block_skips_per_row_checks`、`src/persist/msec/index.rs::ttl_map_roundtrip_and_legacy_tail`、`tests/l5_contracts.rs::ttl_expiry_survives_multi_segment_reopen` | Passed |
| FC-LIFE-CPLX-002 | CPLX | `retain` 扫描:时间 $O(N_{\text{cand}})$(元数据级,不读向量);空间 $O(N_{\text{cand}})$ | 解析证明(07 §3:单遍元数据扫描,不读向量、不重建索引)+ 哨兵 `tests/life_contracts.rs::retain_forgets_below_threshold` | Passed |
| FC-LIFE-CPLX-003 | CPLX | compaction 单轮:时间 $O(S_{\text{merge}}\cdot d\cdot ef_c\cdot M_0)$(重建历史版本建图主导;墓碑/过期/horizon 超期过滤与 delta 合并为线性项);摊还 $O(d\cdot ef_c\cdot M_0\cdot W_{\text{amp}})$;空间峰值 $+O(S_{\text{merge}})$ | 解析证明(07 §4.5:逐行过滤/重写线性,建图主导)+ 哨兵 `tests/l5_contracts.rs::compaction_bounds_segment_count` | Passed |
| FC-LIFE-CPLX-004 | CPLX | 活跃段数 $\le (T-1)\log_r(N/B)+c = O(\log_r N)$(I8);WAL $\le wal\_bytes$(轮转见 FC-PERSIST-POST-011) | 哨兵 `tests/l5_contracts.rs::compaction_bounds_segment_count`、`src/life/compact.rs::plan_merges_smallest_same_level_segments`、`src/life/compact.rs::plan_keeps_levels_separate_and_below_threshold_quiet` + 解析证明(07 §4.2) | Passed |
| FC-LIFE-CPLX-005 | CPLX | `snapshot`:时间 $O(1)$(clone `Arc` 视图);`backup_to`:同盘 $O(\text{files})$(硬链接)、跨盘 $O(\text{bytes})$;`check`: $O(\text{total bytes})$ | 解析证明(03 §4.3:快照 clone `Arc` 视图,与数据量无关;backup/check 逐文件/逐字节遍历)+ 哨兵 `tests/persist_contracts.rs::backup_is_independently_openable`、`tests/persist_contracts.rs::check_detects_corrupt_segment` | Passed |
| FC-LIFE-CPLX-006 | CPLX | 后台维护单轮(与 `Mneme::maintenance_tick()` 手动执行同逻辑):access 攒批落盘 $O(\Delta_{\text{access}})$、retain 扫描 $O(N_{\text{cand}})$、compaction 触发检查 $O(S_{\text{seg}})$;单轮不引入未声明的 $O(N)$ 扫描 | `tests/l5_contracts.rs::access_hits_are_batched_and_flushed`、`tests/l5_contracts.rs::auto_retention_forgets_expired_records`、`tests/l5_contracts.rs::auto_compaction_triggers_in_background` + 解析证明(维护循环逐项按上述规模访问) | Passed |

#### 9.2.7 L6 量化层(quant)

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-QUANT-CPLX-001 | CPLX | i8 量化点积:粗排副本带宽 $4d\to d$ B/行(÷4);VNNI 指令再 $\approx 4\times$;空间副本 $d$ B/行(f32 原向量另存) | 待补 | Planned |
| FC-QUANT-CPLX-002 | CPLX | 两阶段检索:粗排候选 $\le$ `rescore_candidates`(默认 4k);精排时间 $O(k\cdot d)$ | 待补 | Planned |

#### 9.2.8 记忆模型 / 安全 / 部署

| 编号 | 类型 | 形式化规范(时间 / 空间) | 对应测试 / 基准 | 状态 |
|---|---|---|---|---|
| FC-MODEL-CPLX-001 | CPLX | `neighbors(from)`:时间 $O(\log E + degree)$;`predecessors(to)`:默认 $O(E)$ 全段扫描,`RelationIndex::Both` 时 $O(\log E + degree)$;空间 $O(degree)$。注:L1 内存实现为 `HashMap<RowId, Vec<Edge>>`,`neighbors` 期望 $O(1)$+degree(不劣于本条上界)、`predecessors` 全表 $O(E)$;反向表随 L5 落盘(`FC-MODEL-POST-007`) | `tests/model_contracts.rs::predecessors_returns_incoming_edges` | Passed |
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
