# 04 L2 持久层:WAL、段文件与崩溃恢复

> **本章目标**:让"重启不丢数据"成立:定义全部文件格式的**字节级布局**,
> 讲透 WAL 提交/回放、CRC 撕裂写检测、MANIFEST 原子替换与恢复流程。
> **前置阅读**:[00 §6](00-fundamentals.md)(WAL/CRC/MVCC/mmap 科普)、[03](03-l1-memory.md)。
> **本章你将学到**:目录与文件布局 → 四种文件的字节图 → WAL 协议 → CRC 数学 →
> zone map 与 Bloom filter 完整推导 → MANIFEST 原子性(Windows 专项) → 恢复流程。

模块:`persist/{wal.rs, vsec.rs, msec.rs, delta.rs, edges.rs, manifest.rs, recover.rs, flush.rs, source.rs, storage.rs, trash.rs, store.rs}`
(`store.rs` 是协调句柄 `Store`:实现内存引擎的 `PersistHook`、承接 `open`/`flush`/Checkpoint)

---

## 1. 目录布局

```text
agent_memory/
├── current              # 指针文件:内容 = 当前 MANIFEST 版本号(如 "42")
├── MANIFEST.000041      # 旧版本 MANIFEST(write-once,保留最近 2 个)
├── MANIFEST.000042      # 当前版本 MANIFEST
├── wal/
│   ├── wal_000001.log   # 预写日志(可多个,顺序编号)
│   └── wal_000002.log
├── segments/
│   ├── seg_000007.vsec  # 向量段(vectors + norm + 删除位图)
│   ├── seg_000007.msec  # 元数据段(记录体 + 版本链表 + key 索引 + zone map + bloom + 倒排 + ns 统计)
│   └── seg_000007.hidx  # HNSW 图段(L3 起出现;L2 阶段不存在)
└── trash/               # 待物理删除的文件(见 §9)
```

**段(segment)** 是不可变文件三元组:`(vsec, msec[, hidx])` 共享同一 `SegmentId`。
"不可变"是整个存储设计的锚点:只追加新段、只改 MANIFEST 指针,
读路径因此只需短暂读锁换一次视图引用、扫描全程无锁(§8)。可变性由两个例外承担:WAL 阶段的内存可变表(L2 期间)、
后台 compaction 的重写(L5)。

---

## 2. 文件字节布局

约定:整数**小端**(LE);变长字段自带长度前缀;每个文件头部与数据尾部各一个 CRC-32。
头内定长字段按 8 字节自然对齐摆放,便于 mmap 后零拷贝读取。

> **feature 扩展区**:`encrypt` / `compress` 默认关闭。关闭时下述字节图**逐字节成立**;
> 开启后,各文件头在基础字段之后追加一段由 `header_len` 界定的扩展区(§2.5),数据区起点
> 相应后移。旧读者在 feature 关闭时不会遇到扩展字段,兼容性由次版本号保证(§12)。

> **可移植性**:文件格式固定小端。小端平台上 mmap 后可直接零拷贝读取;大端平台需
> 逐字段字节交换(或整体走 `FileSource` 解码路径),正确性不变,但不在性能承诺内。
> 跨架构迁移 = 复制目录即可,格式本身与架构无关。

### 2.1 vsec(向量段)

```text
偏移   字段                          大小
0      magic "VSC1"                  4
4      format_version                u16
6      header_len                    u16
8      dimension                     u32
12     metric                        u8     (0=Cosine 1=Dot 2=Euclidean)
13     quant                         u8     (0=F32;其余值 = 量化副本格式,见 08 §5)
14     norm_col                      u8     (1 = 附带 norm 列)
15     reserved                      u8
16     row_count                     u64
24     created_unix_ms               i64
32     header_crc32                  u32    (覆盖 [0,32))
36     padding to 64B
--- 数据区(每行定长,32B 对齐) --------------------------------------
       vec[0..count]: f32 × dim      (LE,补齐到 32B 对齐)
       norm[0..count]: f32 × count   (norm_col=1 时)
       qvec[0..count]: i8|f16 × dim  (quant != 0 时;量化副本,见 08 §5)
       del_bitmap: 每 1024 行一块,块内 16 个 u64 字(共 1024 bit;1 = 当前不可见:被遮蔽/删除)
--- 尾部 ------------------------------------------------------------
       payload_crc32                 u32    (覆盖整个数据区;可选懒校验)
```

- **32B 对齐**:[02 §4](02-l0-core.md) 的 AVX2 内核用 `loadu` 读取,**不要求** 32B 对齐,
  此处按 32B 摆放只是避免跨缓存行惩罚的优化(非正确性前提);`dim × 4` 不是 32 的倍数时
  每行补齐(pad 区不参与计算);
- **删除位图分块**:查询时按块取"该块是否全活"的单 bit 摘要,整块全活零位图读取;
- **量化副本区**(`quant != 0` 时追加在数据区之后):`qvec[0..count]: i8 × dim`(或 f16 × dim),
  段头附每维 `(v_min, v_max)` 表;f32 原向量**仍保留**在 `vec` 区供精排(两阶段检索,
  见 [08 §2.4/§5](08-l6-quant.md))。因此开启量化后每行总存储 = f32 原向量 + 量化副本,
  副本只降低**查询带宽**,不缩减磁盘占用;
- 物理位置即 `SlotId = 块号 × 1024 + 块内偏移`,位图天然支持 O(1) 判定;
  全局记录标识 `RowId` 由 msec 的 version_table 给出(见 §2.2),用于 `get_by_rowid` 定位。

### 2.2 msec(元数据段)

```text
0      magic "MSC1"                  4
4      format_version                u16
6      header_len                    u16
8      row_count                     u64
16     field_dict_offset / field_dict_len    2×u64  → 字段字典(见下)
32     version_table_offset / version_table_len 2×u64 → 版本链数组(每 RowId 多版本,§5.5)
48     key_index_offset / key_index_len  2×u64 → (NsId, key)→RowId 索引(条目含 SlotId/seqno,§5.5)
64     inv_offset / inv_len          2×u64  → 倒排索引(词表/postings/doc_len,§5.4)
80     ns_stats_offset / ns_stats_len 2×u64 → 命名空间级统计(§5.6)
96     zmap_offset / zmap_len        2×u64  → zone maps(每字段)+ ttl_map(见数据区)
112    bloom_offset / bloom_len      2×u64  → bloom 组
128    delta_offset / delta_len      2×u64  → 跨段覆盖区(墓碑/更新/访问/关系,§2.2a)
144    rel_offset / rel_len          2×u64  → 关系邻接索引(§2.2b)
160    header_crc32                  u32
--- 数据区 ----------------------------------------------------------
       doc_region: 连续的记录体(变长,记录体格式见下;段内顺序即 SlotId 顺序)
       version_table: (RowId u64, seqno u64, tx_ms i64, SlotId u32, doc_offset u64)
                      按 (RowId, seqno) 排序 → 同一 RowId 的全部保留版本(版本链,§5.5);
                      当前可见版本 = 该 RowId 链尾未被墓碑遮蔽者
       key_index:  按 (NsId, key) 排序的 [(NsId u32, key len+bytes, RowId u64, SlotId u32, seqno u64, doc_offset u64)]
                   → get(key) O(log n) 定位本段最新版本(跨段再按 seqno 合并,§5.5)
       field_dict:  [(u16 field_id, len, name bytes)...]  上限 16 个索引字段(默认,见 §5.1)
       zone_maps:   每 1024 行一块 × 每个索引字段: (min, max, has_null)
       (数值/时间字段用 f64/i64 存储;created_at 恒定索引)
       ttl_map:     每 1024 行一块一个 min(expires_at)(无 TTL 行记 +∞)→ TTL 整块剪枝(§5.2);
                    紧接 zone_maps 之后存放,计入 zmap_len
       blooms:      每个高基数字符串字段(含 key)一个 bloom(参数见 §5.3)
       inverted:    倒排索引 = term_dict + postings + doc_len
                    (编码与打分见 [06 §3.4](06-l4-query.md);无文本记录时为空)
       ns_stats:    每命名空间一行 (NsId u32, doc_count u64, total_doc_len u64)
                    → 段内块级剪枝用(§5.6);BM25 的 N/avgdl 需跨段全局聚合
       delta:       跨段覆盖区(§2.2a):墓碑/更新/访问/关系变更的持久化载体,
                    使"作用于旧段记录的操作"不依赖 WAL 存活
       relations:   关系邻接索引(§2.2b):按 (from RowId) 排序的边表,供联想检索
--- 尾部 ------------------------------------------------------------
       payload_crc32                 u32
```

**记录体(entry)格式**(doc_region 内,长度前缀):

```text
[u32 total_len]
[u64 rowid][u64 seqno][u32 ns_id]  # rowid = 稳定逻辑标识(更新时不变,见 §2.3)
[u8 flags]  # bit0=有key bit1=有text bit2=有ttl bit3=有importance bit4=有access
            # bit5=有valid_time bit6=有confidence bit7=有provenance
[key: len+bytes(可选)]
[text: len+bytes(可选)]
[meta: u32 json_len + serde_json 字节]
[i64 created_at_ms][i64 expires_at_ms(可选)][f32 importance(可选)]
[i64 last_access_ms][u32 access_count](可选,bit4;缺省视为 0/0)
[i64 valid_from_ms][i64 valid_to_ms(可选)](可选,bit5;有效时间,见 [09 §3](09-memory-model.md))
[f32 confidence](可选,bit6;默认 1.0)
[provenance: u32 len + JSON 字节](可选,bit7;来源/派生链)
```

> **RowId 是稳定逻辑标识,一个 RowId 可有多个物理版本(版本链)**:更新/upsert 时保留 RowId、
> 递增 seqno 写入新记录体,形成按 seqno 升序的版本链;记录体的 `created_at_ms` 即**事务时间**
> `tx_ms`(经 `Clock` 单调钳制)。`SlotId` 是**物理版本**的段内下标,同一 RowId 的每个保留版本
> 各有自己的 SlotId。`delete` 追加一个**墓碑版本**(`(RowId, seqno, tx_ms)`,无记录体)。
>
> **可见性**:给定水位 `W`(seqno)与事务时间上界 `T`(可选),对每个 RowId 取满足
> `seqno ≤ W 且 tx_ms ≤ T` 的最新版本;若它是墓碑则不可见,否则可见。当前读取 `W` = 视图水位、
> `T = +∞`;`as_of(T)` 取 `T` = 给定时间戳。HNSW 按 SlotId 建节点,搜索时用 **alive 位图**
> 选出上述可见版本([05 §7](05-l3-hnsw.md))。
>
> **保留**:compaction 保留每个 RowId 的最新版本,以及事务时间在
> `CompactionPolicy.history_horizon` 内的历史版本(默认 `None` = **永久保留**);仅超出 horizon
> 的版本被回收。因此 `as_of(t)` 在保留窗口内始终可读,默认永久;需要控制磁盘时设置有限
> horizon(见 [07 §4.2a](07-l5-life.md))。

### 2.2a 跨段覆盖区(delta)

**问题**:`delete(key)` / `update(key, patch)` / `touch` / `relate` 作用的对象可能位于
**更早的不可变段**。这些操作不能原地改写旧段(vsec 的 `dead` 位图只能标本段 SlotId,
msec 记录体也不可变),若只放在内存可变表 + WAL,则 Checkpoint 截断 WAL 后会丢失
(删除"复活"、更新/importance 丢失)。**delta 区把这类"作用于旧段记录的操作"持久化**,
使 WAL 可以安全截断。

```text
delta 区(紧随 msec 数据区,自身带长度前缀与 CRC):
  0  magic "DLT1" | u16 ver | u16 count | u32 delta_crc32(覆盖本条之后的条目区)
  条目 × count(按 (target, seqno) 排序):
    [u8 kind][u64 seqno][i64 tx_ms][u32 ns_id]
    kind=1 DeleteKey   : [key len+bytes]
    kind=2 DeleteRow   : [RowId u64]
    kind=3 UpdateRow   : [RowId u64][u8 field_mask][可选字段,格式同 entry 的对应字段]
    kind=4 Access      : [RowId u64][i64 last_access_ms][u32 access_delta][f32 importance_delta]
    kind=5 Relate      : [from u64][to u64][kind u16][f32 weight][meta len+bytes]
    kind=6 Unrelate    : [from u64][to u64][kind u16]
```

- 读取时,delta 条目与各段记录体、可变表一起参与**统一的可见性合并**(§5.5):同一
  RowId 取最高 seqno;DeleteKey/DeleteRow 遮蔽所有更早版本;Access 覆盖字段;关系边
  叠加到关系邻接索引([09 §2](09-memory-model.md));
- delta 与 msec 同 CRC、同生同灭,因此 delta 一经所在段提交(MANIFEST 生效)即可
  参与 WAL 截断判定(§3.2);
- compaction 时 delta 被**物化**:目标行若仍在活段则应用更新/删除,关系边重建进新段
  relations 区;窗口内的墓碑/更新作为版本链保留,超期条目才随旧段清理;
  多个 delta 区叠加时按 seqno 合并。

### 2.2b 关系邻接索引

联想检索([09 §2](09-memory-model.md))需要"从某条记忆出发找它连到的记忆"。
relations 区是**按 `from` RowId 排序**的边表,供 `O(log n + degree)` 定位:

```text
relations: [from u64][to u64][kind u16][f32 weight][meta len+bytes] × edge_count
           按 (from, kind, to) 排序;另存 from→区间 的稀疏索引(sparse, 每 256 条一个锚点)
           反向边默认不单独存:relations 区按 from 排序,查询 to 的入边只能全段扫描;
           高频入边场景显式开启 RelationIndex::Both,引擎在 relations 区内追加按 (to, kind, from)
           排序的反向邻接索引(区首记录正向/反向两段偏移,不新增 msec 头字段),
           供 ns.predecessors() 以 O(log n + degree) 定位(见 09 §2.3)
```

- 边的可见性同样受 delta 的 Relate/Unrelate 与墓碑约束:任一端被删除,该边在读取时
  视为不可见(悬挂边不返回,compaction 时物理清除);
- 关系边是**一等公民**但不参与向量打分,只作为检索的**扩展算子**([10 §3](10-scoring.md))。

`ns_id` 逐条存储,因为同一段内混合多个命名空间([07 §5](07-l5-life.md) 的单库前缀聚合),
记录体的命名空间不可从段位置推断;它同时是 [16 §8.1](16-api-reference.md) 中保留字段 `__ns` 的
持久化来源。

设计取舍:记录体是**变长 blob 连续排放**——`get(key)` 走 key_index 二分
$O(\log n)$ 一次读;顺序重放/compaction 顺序读,对页缓存与 SSD 都友好。
段内物理槽位 `SlotId` 即记录体在 doc_region 中的顺序号,`version_table` 把它与
全局稳定的 `RowId` 关联起来。

### 2.3 WAL 帧格式

```text
文件头(偏移 0 起,字段按对齐摆放,补齐至 32B):
0   magic "WAL1"(4) | 4 u16 ver | 6 u32 dim | 10 u8 metric | 11 u8 reserved | 12 crc32 | 16..32 保留
之后为帧序列,每帧:
[ u32 crc32 ][ u32 payload_len ][ u64 seqno ][ u8 type ][ payload ]
crc32 覆盖 [payload_len, seqno, type, payload](即本帧除自身 crc 外的全部字节)
seqno 为该写操作分配的全局单调序号(§3.1);回放据此跳过已落段的前缀(§3.3)
type: 1=Insert 2=Delete 3=Touch 4=Checkpoint 5=BatchBegin 6=BatchCommit
      7=DeleteRow 8=TouchRow 9=NsRegister 10=Update 11=UpdateRow 12=Relate 13=Unrelate
      14=RelKindRegister
Insert     = 记录体(同 msec entry 格式,含 NsId)[u32 dim][f32 × dim]  # 记录体不含向量,故附向量副本供崩溃恢复
Delete     = [NsId u32][key len+bytes]
DeleteRow  = [RowId u64]                          # 无 key 记录按 RowId 删除
Touch      = [NsId u32][key len+bytes][i64 at_ms][u32 access_delta][f32 importance_delta]
TouchRow   = [RowId u64][i64 at_ms][u32 access_delta][f32 importance_delta]
NsRegister = [NsId u32][path len+bytes]           # 命名空间注册:首次写入前落帧,保证 path↔NsId 可恢复(§3.3)
Update     = [NsId u32][key len+bytes][u8 field_mask][可选字段,格式同 entry]  # 保留 RowId 的局部更新
UpdateRow  = [RowId u64][u8 field_mask][可选字段]  # 无 key 记录的局部更新
Relate     = [from RowId u64][to RowId u64][kind u16][f32 weight][meta len+bytes]
Unrelate   = [from RowId u64][to RowId u64][kind u16]
RelKindRegister = [kind u16][name len+bytes]      # 自定义关系类型注册:首次使用前落帧,保证编号稳定
BatchBegin = [u32 record_count]                  # 后续连续"数据帧"(Insert/Delete/DeleteRow/Touch/TouchRow/Update/UpdateRow/Relate/Unrelate)属于同一原子批;
                                                 # NsRegister/RelKindRegister 不是数据帧,必须写在 BatchBegin 之前(见下)
BatchCommit= [u32 record_count][u32 batch_crc]   # 批提交标记;缺此帧则整批丢弃
Checkpoint = [u64 watermark_seqno]
```

> **`NsRegister` 的位置保证**:向一个新命名空间写入的**第一批** WAL 必须先写
> `NsRegister`(必须在同批的 `BatchBegin` 之前,或更早),回放时据此重建 `path ↔ NsId` 并推进 `next_ns_id`
> (不变量 I20)。这样即使 `insert` 已 fsync、MANIFEST 尚未更新就崩溃,注册表仍可恢复,
> 且 `NsId` 绝不会因水位回退而被复用。`RelKindRegister` 同理,保证自定义关系类型编号
> 跨崩溃/重启稳定([09 §2.2](09-memory-model.md))。

帧头字段(`crc` + `len` + `seqno` + `type`)是撕裂写检测与恢复定位的关键:恢复时若读不满 `len`、
或 CRC 不符 → 该帧及其后全部丢弃(WAL 是**只追加**的,尾部之后不可能是有效数据)。
`BatchBegin`/`BatchCommit` 让 `insert_batch` 的整批写入在崩溃后要么全部重放、
要么整批丢弃(不变量 I15,回放规则见 §3.3)。
`forget(filter)` / `retain(...)` / `drop_namespace(path)` 的批量墓碑以
`BatchBegin` + 若干 `Delete`/`DeleteRow` + `BatchCommit` 落盘(整批原子);
纯 TTL 过期不写帧——回放后由记录自带的 `expires_at` 逻辑判定,无需持久化墓碑。

### 2.4 MANIFEST

```text
偏移   字段                              大小    说明
0      magic "MNF1"                     4
4      format_version                    u16
6      header_len                        u16     固定头部总长
8      header_crc32                      u32     覆盖除自身外的全部头部字段
12     dimension                         u32     建库维度(空库也可回读;打开时校验)
16     metric                            u8      0=Cosine 1=Dot 2=Euclidean
17     reserved                          5
22     next_rel_kind                     u16     自定义关系类型编号分配水位(永不复用)
24     manifest_version                  u64
32     watermark_seqno                   u64
40     next_rowid                        u64     全局记录标识水位(永不复用)
48     next_segment_id                   u32
52     next_ns_id                        u32     命名空间编号分配水位(永不复用)
56     active_count                      u32
60     ns_count                          u32
64     rel_kind_count                    u32     关系类型注册表条目数
--- 变长区(紧随固定头部) ----------------------------------------------
[ NsEntry ] × ns_count:                  # 命名空间注册表(path ↔ NsId)
    u32 ns_id, u32 path_len, path bytes(UTF-8)
[ RelKindEntry ] × rel_kind_count:       # 关系类型注册表(kind ↔ name,见 09 §2.2)
    u16 kind, u32 name_len, name bytes(UTF-8)
[ SegmentEntry ] × active_count:
    u32 segment_id,
    u16 format_version,                     # 该段文件格式版本(§12;三文件应一致)
    u64 row_count, u64 min_seqno, u64 max_seqno, i64 created_ms,
    u32 vsec_crc, u32 msec_crc, u32 hidx_crc,   # 每文件一个(无 hidx 时 hidx_crc = 0)
    u32 entry_slot, u8 entry_level              # 该段 HNSW 入口(段内 SlotId;L3 起)
尾部: payload_crc32 (覆盖变长区)
```

`dimension` 与 `metric` 是**建库即锁定**的库级属性,由 MANIFEST 作为唯一事实来源持久化
(段文件的 vsec 头也各存一份用于自校验;空库没有段文件,故不能只依赖段头)。
打开时调用方显式传入的维度/度量与此比对,不一致即拒绝([16 §3](16-api-reference.md))。

MANIFEST 是**命名空间路径的唯一事实来源**(`NsEntry` 表);删除命名空间时从表中移除
该路径,但 `next_ns_id` 只增不减,保证 NsId 永不复用([07 §5](07-l5-life.md))。
同理,`RelKindEntry` 表与 `next_rel_kind` 是自定义关系类型名称↔编号的唯一事实来源,
`next_rel_kind` 只增不减,保证关系类型编号跨崩溃/重启稳定([09 §2.2](09-memory-model.md))。
HNSW 入口是**每段一个**(与 [05 §7/§9](05-l3-hnsw.md) 的"每段独立图"一致),不存在全局单入口。

### 2.5 可选 feature 的头部扩展(encrypt / compress)

基础字节图中的头部均有 `header_len`(vsec/msec/MANIFEST)或保留区(WAL),据此承载
可选 feature 的扩展字段。扩展区按固定顺序、定长排布,`header_crc32` 一并覆盖:

| 字段 | 类型 | 缺省 | 含义 |
|---|---|---|---|
| `key_id` | `u32` | `0`(未加密) | 加密时指向 `KeyProvider` 的密钥标识,见 [11 §2.3](11-security-storage.md) |
| `codec` | `u8` | `0`(None) | 记录体 `text`/`meta` 所用压缩 codec(0=None 1=Lz4 2=Zstd),见 [11 §3.2](11-security-storage.md) |

- 扩展字段出现在 **vsec / msec / WAL / MANIFEST** 四类文件头中;同一段的三个文件
  (vsec/msec/hidx)的 `key_id` 与 `codec` 必须一致,不一致视为 `Corrupted`;
- feature 关闭时扩展区为空(`header_len` = 基础值),磁盘布局与 §2.1–§2.4 逐字节一致;
  开启时 `header_len` 增大、次版本号递增,关闭 feature 的旧读者按 §12 跳过未知可选字段;
- 加密页布局与压缩字段前缀的细节见 [11 §2.2](11-security-storage.md) / [11 §3.2](11-security-storage.md)。

### 2.6 一次写入的字节旅程(把本章串起来)

以 `ns.insert(rec)` 为例,标注每一步落在哪个文件、哪些字节(括号为本章小节):

```text
① 校验:维度 / 有限值 / 限额                                [03 §2.1、16 §8]
② 取全局写锁 → 分配单调 seqno                              [§3.1]
③ 编码为 Insert 帧,追加进 WalWriter 缓冲:
     [crc32][payload_len][seqno][type=1][记录体(§2.2 entry 格式)]  [§2.3]
   按 FsyncPolicy 等待 fsync(Always 立即 / Batched 组提交)   [§3.1]
④ 应用到内存可变表:versions / latest / key_index 更新       [03 §3]
⑤ 返回 Ok(此时按策略已持久或未持久,见 I1)                  [§3.1]

后续 flush 与崩溃恢复:
⑥ flush:可变表整体写成新段(seg_N.vsec + seg_N.msec);
   作用于旧段的删除/更新/访问/关系写入该段 delta 区          [§2.1、§2.2a]
⑦ 写 MANIFEST.<v+1>.tmp → fsync → rename → 更新 current     [§2.4、§6]
⑧ Checkpoint 帧记录 watermark_seqno → 截断旧 WAL            [§3.2]
崩溃在任意一步:
   未提交 → 重放 WAL 中 seqno > watermark 的帧               [§3.3、§7]
   已提交 → 覆盖条目已在段 delta 区,删除/更新不丢失(I19)     [§7 算例]
```

**【算例】vsec 头部与数据区的实际字节**:取 `dimension=4`、`row_count=2`、`quant=0`(F32)、`norm_col=1`
(头部 64B;数据区 = vec `2×4×4=32B` + norm `2×4=8B` + del_bitmap 首块 `128B`;尾部 CRC 4B):

```text
偏移   内容
0      "VSC1" = 56 53 43 31
4      01 00                        format_version = 0x0001
6      40 00                        header_len = 64
8      04 00 00 00                  dimension = 4
12     00                           metric = 0(Cosine)
13     00                           quant = 0(F32)
14     01                           norm_col = 1
16     02 00 00 00 00 00 00 00      row_count = 2
24     ... created_unix_ms(i64)
32     ... header_crc32(覆盖 [0,32))
36     ... padding 至 64B
64     vec[0] 4 个 f32(LE);vec[1] 紧随其后(共 32B)
96     norm[0]、norm[1](各 1 个 f32,存范数平方,共 8B)
104    del_bitmap 首块:16 个 u64(1024 bit;行 2 起为未用槽,恒 1=不可见)
232    payload_crc32(覆盖 [64,232))
```

文件总大小 = 64 + 32 + 8 + 128 + 4 = **236 字节**。维度越大,`vec`/`norm` 区按 `d` 线性增长,
而 `del_bitmap` 只随行数增长——这就是"块粒度 1024 行"固定不变的原因。

---

## 3. WAL 协议

### 3.1 提交流程(写路径)

```text
1. 取写锁(全局串行) → 为本次写操作分配全局单调 seqno(批内各帧依次分配;
   flush 以批为单位,保证一批不横跨 watermark)
2. 构帧追加进 WalWriter 的缓冲区:单条写 = Insert/Delete/Touch;
   `insert_batch` = BatchBegin + N×Insert + BatchCommit(整批一次组提交)
3. 按 FsyncPolicy 等待持久确认:
     Always      → 每帧 write + fsync 后返回
     Batched(d)  → 写线程每 d 毫秒统一 write+fsync;提交者等待"自己的写入已 fsync"事件(水位 last_durable ≥ my_seqno)
     OnFlush     → 不等待(由 flush 阶段统一 fsync),断电语义 = Batched(∞)
     Never       → 只写页缓存(测试专用)
4. 应用到内存可变表 → 返回 InsertOutcome
```

`Batched` 的实现:`Mutex<WalState> + Condvar`;`WalState { buf, last_durable_seqno,
last_error }`。提交者追加后 `wait_while(last_durable < my_seqno)`。
这样 N 个并发写者共享一次 fsync 的成本——**组提交(group commit)**,
是 WAL 吞吐的标准手法。

### 3.2 轮转与检查点

- WAL 文件达 **64MB(默认)** → 换新文件,旧文件保留到 Checkpoint;
- **整批不跨文件提交**:轮转只在帧边界发生,且 `BatchBegin…BatchCommit` 必须完整落在
  同一个 WAL 文件内——若追加过程中将触达文件上限,先写完当前批再轮转。由此批原子性
  (I15)不依赖跨文件逻辑,回放器也只需单文件即可判定整批取舍;
- **Checkpoint(type 4)**:只有当**内存可变表 + 所有 seqno ≤ watermark 的覆盖操作**
  (Delete/DeleteRow/Update/UpdateRow/Touch/TouchRow/Relate/Unrelate)都已随某个已提交
  段的记录体、**version_table 版本链**或 delta 区(§2.2a)**持久物化**,才写入 Checkpoint
  并记录 `watermark_seqno`;此后 `seqno ≤ watermark` 的 WAL 文件整体删除。
  **这是防止"删除复活/更新丢失"的关键**(不变量 I19):只要某条墓碑或更新还只存在于 WAL,
  就不得截断它。
- **L2 阶段兜底**(还没有 compaction):WAL 总量超限(默认 256MB)即触发一次
  "全量快照 flush"(把可变表整体写成一个段,并把覆盖操作写入该段 delta 区),保证 WAL 有界。
  L5 之后该兜底由正规 compaction 取代。

### 3.3 回放(恢复路径的一部分,见 §7)

按序读帧 → 校验 `len`/`crc` → 应用
`type ∈ {Insert, Delete, DeleteRow, Touch, TouchRow, Update, UpdateRow, Relate, Unrelate}`
到内存可变表/覆盖层 → 遇坏帧即停并截断文件。**只回放 `seqno > manifest.watermark_seqno`
的帧**——每帧头部都带 seqno(§2.3),`≤ watermark` 的操作已随段提交落盘。这样即使崩溃
发生在"MANIFEST 已提交、Checkpoint 帧尚未写入"的窗口,也不会重复应用已落段的操作。

**注册与水位恢复**:回放遇到 `NsRegister` 时把 `(NsId, path)` 并入内存注册表,并令
`next_ns_id = max(next_ns_id, NsId + 1)`;遇到 `RelKindRegister` 时把 `(kind, name)` 并入
关系类型注册表,并令 `next_rel_kind = max(next_rel_kind, kind + 1)`;
遇到任意 `Insert`/`UpdateRow`/`Relate` 时,令 `next_rowid = max(next_rowid, rowid + 1)`。
回放结束把注册表与水位写回 MANIFEST(不变量 I20)。这保证 MANIFEST 尚未更新就崩溃时,
路径映射、关系类型与 ID 水位仍可精确恢复。

**批原子回放**:遇到 `BatchBegin` 时把批内后续帧(上述全部数据帧)先暂存,直到读到配对的
`BatchCommit`(校验 `batch_crc` 与条数)才一次性应用;若在提交帧前遇坏帧/文件结束,
整批丢弃——由此保证 I15(验收见 [14 §2.1](14-testing.md))。

### 3.4 【复杂度】

| 操作 | 时间 | 磁盘 |
|---|---|---|
| 单条提交(Batched,组满) | $O(1)$ 内存追加 + 等待 | 顺序写 |
| 单条提交(Always) | 1 次 fsync ≈ 0.1–10ms(经验值,盘型决定) | 顺序写 |
| 组提交 N 条/批 | $O(N)$ 追加 + **1 次** fsync | 顺序写 |
| 回放 | $O(\text{unflushed records})$ | 顺序读 |

---

## 4. CRC-32:撕裂写的守门人

### 4.1 【直觉】数据的"指纹"

把任意字节流交给一个约定好的除法机器,吐出一个 32 位余数——这就是 CRC。
数据哪怕翻转 1 个 bit,余数都会变。存储时把"数据 + 余数"一起写;
读取时重算余数对不上 → 数据必然坏了。

### 4.2 【数学】多项式除法视角

把字节流看作 GF(2)(系数为 0/1 的域,加减即 XOR)上的多项式 $M(x)$ 的系数,
选定**生成多项式** $G(x)$(CRC-32/IEEE:$x^{32}+x^{26}+x^{23}+\cdots+1$,
简化记法 `0x04C11DB7`,反射实现 `0xEDB88320`)。发送前计算:

$$R(x) = \left( M(x) \cdot x^{32} \right) \bmod G(x)$$

传输 `M·x³² + R`(补余数使整体被 G 整除);接收端重除,G(x) 首尾项保证:
若余数 ≠ 0 → 必有错。**检错能力**(代数可证):所有长度 ≤ 32 bit 的
**突发错误**(burst,连续错误段)全部可检出;1 bit 错误 100% 检出。
注意:CRC-32/IEEE 的生成多项式不含因子 $(x+1)$,因此**不**保证检出所有奇数个
bit 错误(需要该性质时应选用含 $(x+1)$ 因子的多项式)。

**【算例】** 用小型生成多项式 $G = x^3 + x + 1$(二进制 `1011`)手算一次:
数据 `M = 1101`,左移 3 位得 `1101000`,对 `G` 做模 2 除法(逐位异或,无进位):

```text
  1101000
⊕ 1011000   ← G 与当前最高位 1 对齐
  -------
  0110000
⊕  0101100  ← 对齐下一个 1
  -------
   0011100
⊕   0010110
  -------
    0001010
⊕    0001011
  -------
     0000001  ← 余数 = 001
```

余数 `001` 即 3 位校验码,发送 `1101001`;接收端再除 `1011` 余数为 0。
把任意一位翻转(如 `1101101`)重算,余数非 0 → 检出。Mneme 用 32 位版本
(CRC-32,4 字节校验码),原理相同、只是位宽更大。

### 4.3 【工程】在 Mneme 里的四处岗哨

1. WAL 每帧:防撕裂写(读不满/CRC 不符 → 截断);
2. 文件头:防元数据损坏;
3. vsec/msec 尾部 payload CRC:**启动时可配校验**(`Builder::verify_on_open(true)`;默认只校验头部,全量校验走 `db.check()`,
   1GB 段全量 CRC ≈ 1s 量级,启动时间不为其买单);
4. MANIFEST payload:MANIFEST 坏 → 回退旧版本(§6)。

CRC 是**意外损坏**检测,不是防篡改(它可被伪造);超长期数据的完整性靠
"多版本 MANIFEST + 备份"而非加密哈希——诚实标注其边界。

### 4.4 复杂度

$O(n)$ 时间(查表法每字节 1 次表查 + XOR,8KB 表)、$O(1)$ 空间;`crc32fast`
用 SIMD 切片算法,吞吐 ~GB/s(经验值)。

---

## 5. msec 内的轻量索引

### 5.1 字段字典

每段维护"字段名 → field_id(u16)"字典,**上限 16 个可索引字段**(默认;
`created_at` 恒定占用其一)。元数据是开放 JSON,但**过滤热词**高度集中
(kind/agent_id/session_id/importance/created_at…),16 个字典槽覆盖绝大多数负载;
超出的字段仍可过滤(逐行求值兜底),只是没有下推加速。

### 5.2 zone map(分块 min/max 索引)

**【直觉】** 把 1024 行划成一块,块级记下每个数值字段的 min/max——像书架每层
贴着"本层书籍价格区间 ¥20–¥80"。找"价格 > ¥500"的书,整层直接跳过。

**【数学】** 对谓词 `f op v`(op ∈ {>,≥,<,≤,==})与块区间 [min,max]:
- `op 为 >`:块**不可能**有匹配,当 `max ≤ v`;**必全**匹配,当 `min > v`(可整块置位);
- `op 为 ≥`:块不可能有匹配,当 `max < v`;必全匹配,当 `min ≥ v`;
- `op 为 <`:块不可能有匹配,当 `min ≥ v`;必全匹配,当 `max < v`;
- `op 为 ≤`:块不可能有匹配,当 `min > v`;必全匹配,当 `max ≤ v`;
- `op 为 ==`:`v ∉ [min, max]` → 整块剪枝;`min = max = v` 时必全匹配。
- 代价:每块每字段 16 字节(两个 8 字节 min/max)+ 1 bit has_null;
- 复杂度:全块扫描 $O(\lceil N/1024 \rceil)$ 次**内存连续**判断——1M 行 = 977 次
  判断,亚微秒级;被剪块内的行完全不读。
- **TTL 剪枝**:除字段 zone map 外,每块另存一个 `min(expires_at)`(§2.2 的 `ttl_map`)。
  查询时若 `min(expires_at) > now`,整块无过期行、零逐行判断([07 §1](07-l5-life.md))。

**【算例】** 块内 `importance ∈ [0.10, 0.42]`,谓词 `importance > 0.5`:
`max 0.42 ≤ 0.5` → 整块剪除,块内 1024 行零评估。

### 5.3 Bloom filter(字符串等值预筛)

**【直觉】** 一亿条记忆里问"含 'alpha' 这个词的有没有?"——逐条查太慢。
布隆过滤器是个"指纹册":每个值用 k 个哈希函数在位图上戳 k 个洞;
查询时看这 k 个位置:**只要有一个空 → 必定不存在;全满 → 大概率存在**。
不漏报、允许极低误报,空间与值的长度无关。

**【数学】** m bit 位图、n 个元素、k 个独立均匀哈希。某个特定 bit 在插入 n 个元素后
仍为 0 的概率:单次插入不碰它的概率是 $1 - 1/m$,独立重复:

$$P(\text{bit}=0) = \left(1 - \frac{1}{m}\right)^{kn} \approx e^{-kn/m}$$

误判率(查一个未插入的元素,k 个位置全被别人占满):

$$p = \left(1 - e^{-kn/m}\right)^{k}$$

对 k 求导取最优 $k^{*} = \dfrac{m}{n}\ln 2$,代回得空间-误判率关系:

$$\frac{m}{n} = \frac{\log_2 (1/p)}{\ln 2} \approx 1.44 \, \log_2(1/p) \ \text{bit/element}$$

**【算例】** n = 10 万,p = 1%:$m/n = 1.44 \times \log_2 100 \approx 9.57$ bit/元素
→ $m \approx 9.6 \times 10^5$ bit ≈ **120 KB**,$k^{*} = 9.57 \times 0.693 \approx 6.6 \Rightarrow k = 7$。
代入验证:$p = (1-e^{-7\times10^5/(9.57\times10^5)})^7 = (1-e^{-0.73})^7 \approx 0.010$ ✓

**【工程】** 哈希用**双哈希法**(Kirsch–Mitzenmacher):只需两个 64 位哈希 $h_1, h_2$
(取自 crc32 组合),第 i 个位置 $h_i(x) = h_1(x) + i \cdot h_2(x) \bmod m$——
k 次哈希的成本变成 2 次哈希 + k 次乘加。误报的后果只是"多评估几行",**安全性无害**。
Mneme 在 msec 每段每字符串字段放一个 bloom(fpp 1%,默认),供等值过滤下推使用;
范围过滤走 zone map,两者互补。

### 5.4 倒排索引(为 BM25 供数据)

有 `text` 的记录在 flush 时顺带构建倒排(词表 → postings 差分序列 → doc_len 列),
编码细节与 BM25 打分见 [06 §3.4](06-l4-query.md)。此处只定两条规则:

1. 倒排是 **msec 的一部分**(同一 CRC 保护、同生同灭),不是独立文件——
   保证"向量、元数据、文本索引"三者永远一致(同一 MANIFEST 版本 = 同一批物理行);
2. 段内无文本记录时该区域为空(`inv_len = 0`),零开销;
3. **未落段记录**:可变表在内存中维护一份**增量倒排**(词 → 未落段 RowId 列表,插入/更新时增量维护,
   flush 后并入新段倒排并清空);BM25 查询把它当作"内存段"与各段倒排一起参与两遍统计与打分
   ([06 §3.2](06-l4-query.md)),因此新写入的 `text` 无需 `flush` 即可被检索。

### 5.5 key 索引(为 `get(key)` 与去重供路)

每段在 msec 内维护按 `(NsId, key)` 排序的 key 索引 `[(NsId, key, RowId, SlotId, seqno, doc_offset)]`(§2.2):
`get(key)` 二分 $O(\log n)$ 命中当前版本,再经 version_table 取该 RowId 的版本链。
key 同时进入该字符串字段的 bloom(§5.3),不存在的 key 先被 bloom 挡掉,
避免无谓的索引查找。超期版本在 compaction 时物理剔除;墓碑版本由可见性规则过滤。

**跨段版本链解析与墓碑覆盖**(`get`/`delete`/`touch`/upsert/`as_of` 的共同规则):

```text
一次 get(key) 或 as_of(T).get(key) 需要看三个来源:
  a) 各活跃段记录体: 对每段 version_table 二分定位该 RowId 的版本链;
  b) 各活跃段 delta: 段内 delta 区的 DeleteKey/DeleteRow/UpdateRow/Access(§2.2a),
                     均带 (seqno, tx_ms);
  c) 可变表:         内存版本链 + 尚未 flush 的覆盖操作。
可见性解析(给定 W = 水位, T = 事务时间上界;当前读 T = +∞):
  1. 把 a/b/c 中同一 RowId 的全部版本按 seqno 升序合并成一条版本链;
  2. 取满足 seqno ≤ W 且 tx_ms ≤ T 的最新版本(记录体或墓碑);
  3. 若该版本是 DeleteKey/DeleteRow 墓碑 → 该 RowId 不可见;
     否则对该版本叠加 UpdateRow/Access 字段覆盖,得到可见记录;
  4. seqno 相同不可能(全局单调分配),故无并列。
```

- upsert/update **保留 RowId**、追加新版本(seqno 更大)
  ([03 §2.1](03-l1-memory.md)),新旧版本可能落在不同段;
  上述规则保证 `get(key)` 总返回该 RowId 的最新可见版本;
- `as_of(T)` 取 `T` = 给定事务时间戳,由版本链直接解析出历史可见版本;
  compaction 按 `history_horizon` 回收超期版本(§2.2、[07 §4.2a](07-l5-life.md));
- `delete(key)`/`touch(key)` 追加按 `(NsId, key)` 的墓碑/更新版本进可变表与 WAL
  (§2.3 的 Delete/Touch 帧),读取时**遮蔽所有更早版本**;
- 无 key 的记录不参与本规则,只能经 `RowId`(`get_by_rowid`)或过滤器访问
  ([16 §1.3](16-api-reference.md));
- 代价:$O(S)$ 次段内二分($S$ = 活跃段数 $\le O(\log N)$,[07 §8](07-l5-life.md)),
  bloom 先行否定可把绝大多数未命中 key 降到近零成本。

### 5.6 命名空间级统计(为 BM25 供正确的 N/avgdl/df)

段内混装多个命名空间([07 §5](07-l5-life.md)),而 BM25 的 IDF 依赖**查询命名空间内的全局
文档集**。两个层次必须分清:

- **段内统计 `ns_stats: (NsId, doc_count, total_doc_len)`**(§2.2)只用于**块级剪枝/跳过**
  (段内该 NS 无文档则整段跳过),**不能直接当 BM25 的 N/avgdl 用**;
- **全局统计**:$N$ = 查询命名空间在**所有活跃段**的 `doc_count` 之和;avgdl 由全局
  `total_doc_len / N` 得到;$df_t$ = 该词在查询命名空间内的**跨段**文档频率,且**只计活行**
  (排除墓碑/被更新遮蔽的旧版本)。

**查询时的两遍法**(成本极低,见 [06 §3.2](06-l4-query.md)):

```text
第 1 遍(统计):对每个查询词,并行遍历各段 term_dict/postings,
              累加 df_t(按 ns_id 过滤、按 alive 位图排除死行)与各段 ns_stats;
第 2 遍(打分):用全局 N/avgdl/df_t 对同一批 postings 打分并取各段 TopK。
```

- 第 1 遍只读 term_dict 与 postings 的 `ns_id`/存活位,不物化记录;若某段 term_dict 无该词,
  bloom/term_dict 二分即可跳过;
- 只有查询命名空间完全相同的统计才可复用;**不变量 I21(BM25 统计一致性)**:IDF 的 N/avgdl/df
  按查询命名空间在**全部活跃段**全局聚合、只计活行,与段数无关,且不同命名空间的统计互不影响;
- 若未来引入"全局词表"(跨段合并的 term 统计缓存),可把两遍降为一遍,但语义以本规则为准。

---

## 6. MANIFEST 原子性:write-once + 指针(Windows 专项)

**问题**:MANIFEST 更新必须原子。Unix 上 `rename(tmp, dest)` 可原子覆盖目标;
Windows 的 std `rename` 虽对应 `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`、对已存在的
普通文件也能覆盖,但替换语义并不可靠——目标被其他句柄打开(共享冲突)或带只读属性时
会失败,而 `ReplaceFileW` 需要额外 unsafe/依赖,违反依赖白名单。为保证任意一步崩溃后
仍有一致可用的 MANIFEST,干脆不依赖"覆盖"这条路:

**方案:write-once 版本文件 + 指针**:

```text
提交 v42:  ① 写 MANIFEST.000042.tmp → fsync
           ② rename → MANIFEST.000042      (目标不存在,rename 原子且平台通用)
           ③ fsync 目录
           ④ 写 current.tmp("42") → rename → current
读路径:    读 current → 打开 MANIFEST.<v> → 校验 CRC
           失败 → 扫描目录所有 MANIFEST.*,取"最大的、CRC 合法的"版本
保留策略:  始终保留最近 2 个版本 → 任意一步崩溃都至少有一个完整可用 MANIFEST
```

每次提交 = 一个新文件,**永不原地覆盖**;这正是 [00 §6.6](00-fundamentals.md)
"读端无锁"的基础:读者打开一个 MANIFEST 版本后,其引用的段文件都不可变,
整个读路径与写路径零共享可变状态。

> **`current` 是唯一的原地替换点**:它内容极小(版本号字符串),通过
> `写 current.tmp → rename 覆盖` 提交。为规避 Windows 上"目标被打开则 rename 失败"
> 的问题,读端(含只读实例)探测 `current` 时**读完即关闭句柄**,不长期持有;
> 若替换仍失败(句柄未释放),提交方按指数退避重试,期间旧 `current` 始终可用,
> 不会出现"无 current"的中间态。

---

## 7. 恢复流程:`recover.rs`

```text
open(dir):
 1. 读 current → 定位 MANIFEST.<v>;CRC 失败 → 扫描取最大合法版本;全坏 → 报 Corrupted
 2. 校验各段头部 CRC(全量 CRC 视配置);头部损坏段 → 移入 trash/ 并从视图剔除
    (可配 fail-fast 模式:`Builder::fail_fast_on_corruption(true)`,直接报错拒绝启动)。只读实例(`read_only`)不写盘,
    仅跳过损坏段并从视图剔除,经 `stats()`/`check()` 报告
 3. 打开 WAL 文件(按编号序),只重放 seqno > manifest.watermark_seqno 的帧(§3.3);
    遇撕裂帧 → 可写实例截断该文件尾部;只读实例(`read_only`)不写盘,仅在内存中忽略
    该帧及其后(见 §13 只读模式)
 4. 构建 ReaderView{ manifest 版本, 可变表 }:合并各段 version_table/delta 与 WAL 覆盖,
    重建每个 RowId 的版本链与当前可见版本(§5.5) → 对外服务
不变量:恢复后的状态 = "已 fsync 确认的全部操作" 的重放结果(可多,不可错;
       Batched 策略下多出的只能是被 OS 已刷盘但确认事件未送达的帧,重放是幂等的)
```

**段生命周期(STA 契约用语,对应 [FC-PERSIST-STA-001](../spec/contracts.md))**:
- `Building`:正在写入的段(临时名,未进任何 MANIFEST 视图);
- `Committed`:已随某个 MANIFEST 版本提交、内容不可变;
- `Obsolete`:已被更新版本的 MANIFEST 替换、不再被任何活视图引用;
- `(trash)`:已 rename 进 `trash/`、等待最后一个读者释放后物理删除。
崩溃点若落在 `Building`,该段是孤儿,恢复时清理(不进入任何 MANIFEST 视图)。

```mermaid
stateDiagram-v2
    [*] --> Building: 新建 / 写入中
    Building --> Committed: MANIFEST 提交
    Committed --> Obsolete: 新 MANIFEST 替换
    Obsolete --> Trash: rename → trash/
    Trash --> [*]: 最后一个读者释放后删除
    Building --> [*]: 崩溃 → 恢复时清理孤儿段
```

**【算例】崩溃窗口与水位回放**:写操作 `seqno 100–105` 已 fsync 进 WAL;`flush` 把可变表
写成段并提交 `MANIFEST.000042`(其 `watermark_seqno = 105`);此时进程崩溃,`Checkpoint`
帧尚未来得及写入。重启时:

```text
读 current → MANIFEST.000042(watermark = 105)
只回放 WAL 中 seqno > 105 的帧 → 100–105 不会被重复应用
删除/更新等覆盖条目已随该段 delta 区落盘 → 截断 WAL 也不会"复活"或丢失(I19)
```

若崩溃发生在"WAL 已写、MANIFEST 未提交"的窗口,watermark 仍是旧值,`100–105` 会被完整
回放——结果同样是"已确认操作的前缀",只是这次靠 WAL 而非段。

---

## 8. 读路径:ReaderView 与快照

```text
Reader = RwLock<Arc<ReaderView>>   ← [01 §2.1] 的具体形态
ReaderView(不可变):
  segments: Arc<[SegmentHandle]>     # 每段持有 source(Arc<mmap 或 File>) + delta 区
  mutable: Arc<MutableSnapshot>      # 取视图时可变表+覆盖层的不可变快照(WAL 已应用部分)
  watermark: SeqNo
读: 拿读锁 clone Arc → 放锁 → 段扫描 + delta/可变表覆盖 → 全局归并(§5.5)
删除/更新/插入: 修改可变表覆盖层 → flush 时生成新段(含 delta 区) → 提交新 MANIFEST → 写锁内换 Arc
```

> **覆盖层是可持久的**:视图里的墓碑/更新既来自 WAL 重放,也来自各段已提交的 delta 区
> (§2.2a)。因此 `flush` 之后即便 WAL 被 Checkpoint 截断,删除与更新依然有效(I19)。

> **可变表也是检索数据源**:除可见性合并外,可变表中**尚未落段**的记录同时参与检索——
> 向量侧作为一个"内存段"参与暴力扫描([05 §9](05-l3-hnsw.md)),BM25 侧经内存增量倒排
> 参与两遍统计(见 §5.4、[06 §3.2](06-l4-query.md))。因此新写入无需
> 先 `flush` 即可被 `search()` 命中;`flush` 只是把可变表转成不可变段、降低内存占用。

旧视图因 `Arc` 仍被读者持有而存活——**读者永远看到一致的过去**,
这正是 MVCC 水位的工程形态。`SnapshotHandle` 即"钉住一个 `ReaderView`":
它同时包含当时的段集**与可变表快照**,因此能看到取快照前所有已确认写入,
无需先 `flush`([07 §6](07-l5-life.md))。

---

## 9. trash:Windows 上的延迟删除

Windows 不允许删除被 mmap/句柄打开的文件。方案:

```text
提交新 MANIFEST 后: 旧段文件 rename → trash/<name>(rename 打开中的文件是允许的)
登记待删表 {name → 引用计数};最后一个读者 Drop 时(或下次 open 时)尝试删除;
删不掉(仍被旧视图引用)→ 留在 trash,下次启动再试。
```

磁盘占用上界:trash ≈ 一轮 compaction/flush 的增量(经验值:总量的 5–15%),
启动时清理保证不累积。

---

## 10. 测试注入点:`FsyncHook` 与 `Clock`

### 10.1 崩溃注入:`FsyncHook`

```rust
/// 待注入的 I/O 动作。
pub enum IoAction {
    Write { file: &'static str, offset: u64, len: usize },
    Fsync { file: &'static str },
    Rename { from: &'static str, to: &'static str },
}

pub trait FsyncHook: Send + Sync {
    /// 在每次 write/fsync/rename 前调用;可注入故障(丢写/翻转字节/截断/崩溃)
    fn before(&self, action: IoAction) -> std::io::Result<()>;
}
```

经 `Builder::fsync_hook` 公开注入(测试用 seam;生产不设置即无开销)。
用法见 [14 §2](14-testing.md):在随机点"杀死"进程,断言恢复后状态 = 已确认操作前缀。

### 10.2 时间源:`Clock`

TTL 换算、遗忘曲线评分、`touch` 的 `last_access` 全部经 `Clock` 取"当前 Unix 毫秒"
([02 §8](02-l0-core.md)):

```rust
pub trait Clock: Send + Sync { fn now_unix_ms(&self) -> i64; }
```

- 生产用 `SystemClock`(读系统墙上时钟);测试注入假时钟即可确定性地
  "快进 30 天"验证 TTL/遗忘,无需 `sleep`;
- **时钟回拨**:若 `now < last_observed`,引擎以单调水位钳制(取历史最大值),
  避免记录因回拨被误判过期——TTL 只可能"晚消失",不可能"早消失"
  ([07 §1](07-l5-life.md));
- 仅影响时间语义,不影响 WAL 的 seqno(seqno 与时钟无关)。

## 11. source.rs:两种段读取后端

```rust
pub trait SegmentSource: Send + Sync {
    fn slice(&self) -> Option<&[u8]>;                 // mmap 后端返回整段切片
    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<()>;
}
```

L2 提供 `FileSource`(std,`seek+read`);`MmapSource`(feature `mmap`,memmap2)按依赖
白名单([01 §5](01-overview.md))自 **L3** 引入。索引层与恢复层只依赖此 trait——
mmap 是**优化**而非功能依赖,任何平台不支持时可整体退化为 `FileSource`(读吞吐降,正确性不变)。

---

## 12. 格式版本与迁移

每个文件头都带 `format_version`(本章 §2),MANIFEST 另记录各段的版本。规则如下:

| 场景 | 行为 |
|---|---|
| `major(found) ≤ major(max_supported)` | 正常打开;旧主版本由后台 compaction 在重写时升级到当前版本;次版本更高时忽略未知可选字段 |
| `major(found) > major(max_supported)` | 返回 `UnsupportedVersion { file, found, max }`,**拒绝打开**(I18) |
| 魔数不符 | 视为损坏,走 §7 隔离/拒绝流程 |

- **版本号策略**:`format_version` 是 16 位,**高 8 位为主版本、低 8 位为次版本**
  (如 `0x0201` = 主 2 次 1)。破坏性布局变更 → 主版本 +1 且次版本归零;
  仅新增可选字段/保留位 → 次版本 +1,旧读者可安全忽略。兼容判定看**主版本**:
  主版本 ≤ 本库支持上界即可读;次版本只允许追加**带长度前缀、可安全跳过**的可选字段,
  故次版本更高时按"跳过未知可选字段"处理;若同主版本文件仍出现无法跳过的未知布局
  (违反版本号约定),视为 `Corrupted`/`UnsupportedVersion` 并拒绝,绝不猜测解析;
- **升级不阻塞**:打开旧库无需等待全量重写,读路径兼容旧段,新写入与 compaction
  产生新版本段,库内可短暂**混合版本**;全部段升级完成后 MANIFEST 只含新版本;
- **不支持降级**:新版库不能被旧版 Mneme 打开——升级前先备份;
- 迁移在后台 compaction 中随段重写推进,无需人工干预;`db.stats()` 的 `compaction`
  字段可见后台任务是否在运行。

## 13. 失败模式与处置

| 故障 | 触发 | 引擎行为 | 调用方处置 |
|---|---|---|---|
| 磁盘满(ENOSPC) | write/fsync 返回错误 | 写入返回 `Io`;compaction **暂停**而非损坏;WAL 不推进 | 清理 `trash/` 或扩容,重试 `flush()` |
| 只读文件系统 / 只读模式 | `read_only(true)` 打开 | 打开成功(不创建锁文件);任何写操作返回 `Unsupported { feature: "只读模式写入" }`(模式检查先于 I/O) | 换可写目录或保持只读 |
| 只读文件系统 | 可写打开 | 创建锁文件失败 → `Io`(未创建任何数据) | 换可写目录或改用 `read_only(true)` |
| 目录被占用 | 第二个实例打开 | `Busy` | 确保单进程独占([16 §3](16-api-reference.md)) |
| 全部 MANIFEST 损坏 | 扫描 `MANIFEST.*` 无合法版本 | `Corrupted` | 从备份恢复([16 §7](16-api-reference.md)) |
| 个别段头损坏 | 打开时校验失败 | 移入 `trash/` 并从视图剔除(可配 fail-fast,见 §7) | `db.check()` 复核;必要时从备份补段 |
| WAL 未知帧类型 | 回放遇到 `type` 不在定义内 | **停止回放并报错**(不静默跳过) | 升级库版本;切勿手工改 WAL |
| mmap 失败 | 平台/文件系统不支持 | 自动退化为 `FileSource` | 无(功能不变,吞吐下降) |
| 时钟回拨 | `Clock` 返回变小 | 以历史最大水位钳制(本章 §10.2) | 无 |
| 崩溃残留锁文件 | 上次进程未正常退出 | 打开时校验锁内 PID/时间戳,陈旧则接管([16 §3](16-api-reference.md)) | 无(自动接管) |

**原则**:任何失败都不得静默返回错误数据(I2);宁可拒绝启动或返回 `Err`,
也不做"尽力而为"的猜测。用户侧完整 runbook 见 [16 §7](16-api-reference.md)。

## 14. 层边界契约(L2 → 上层)

**向上提供**:

1. 持久化的 `Mneme` 引擎——与 L1 完全相同的公开签名(见 [03 §8](03-l1-memory.md)),
   不再是易失内存实现;`VectorStore` 内部 trait 按 [03 §8](03-l1-memory.md) 推迟到 L3
   随 HNSW 引入,故本层不新增该 trait 的实现分歧;
2. 段格式编解码(`vsec/msec/wal/manifest` 的 read/write/replay,含 version_table 版本链,字节布局本章 §2);
3. `SegmentSource` 抽象、`FsyncHook` / `Clock` 注入点、`trash` 管理;
4. 事务语义:单次 `insert_batch` 原子(要么整批进 WAL,要么整批不出现);
   flush + MANIFEST 提交 = 一致性点;
5. 格式版本校验与在线迁移(§12)、失败模式处置约定(§13)。

**不变量**(任何上层、任何测试可依赖):

- I1 已确认写入不出现半写:未确认写入,重启后**要么完整可见要么不存在**。持久性按
  `FsyncPolicy` 分级——`Always`/`Batched` 下"返回 Ok"即已 fsync,重启后可见;
  `OnFlush`/`Never` 下"返回 Ok"不蕴含已落盘,可见性以最近一次 `flush()`/`close()` 为界;
- I2 任意文件任意 bit 损坏,可被检出(或拒绝启动),绝不静默返回错误数据;
- I3 活跃段集合任意时刻 = 某 MANIFEST 版本所列集合(读端无锁的前提);
- I4 WAL 总量有界(§3.2 兜底),段文件只增不改(write-once);磁盘占用随保留的历史版本
  线性增长,由 `history_horizon` 控制(默认永久);
- I18 只接受 `major(format_version) ≤ major(max_supported)` 的文件,主版本更高的拒绝打开(§12);
- **I19 覆盖持久性**:任何已返回 `Ok` 的 `delete`/`update`/`touch`/`relate` 操作,在任意
  崩溃 + WAL 截断后仍然生效——因为其覆盖条目要么在 WAL,要么已在某个已提交段的 delta 区;
  Checkpoint 绝不在覆盖条目物化前截断 WAL(§3.2);被删除记录**永不复活**(验收 [14 §2.4](14-testing.md));
- **I20 注册与水位可恢复**:`path ↔ NsId` 映射与 `next_ns_id`/`next_rowid` 水位可由
  "MANIFEST + WAL 中 `NsRegister`/数据帧"完整重建,NsId/RowId 永不复用(§3.3);
- **版本链保留(I26 的存储侧保证)**:每个 RowId 的最新版本与事务时间在
  `CompactionPolicy.history_horizon` 内的历史版本(默认 `None` = 永久)被保留;
  版本链在重启/compaction 后可由 `version_table` + delta 重建,`as_of(t)` 据此解析(§2.2、§5.5)。

## 本章小结

- 目录布局 + `vsec/msec/hidx/MANIFEST/WAL` 的**字节级格式**是本层的核心产出。
- WAL 组提交、帧 CRC、`BatchBegin/Commit`、Checkpoint 与 watermark 保证崩溃一致性。
- **delta 覆盖区**把"作用于旧段记录"的删除/更新/访问/关系持久化,是 I19 的关键。
- MANIFEST 用 write-once + 指针规避 Windows 替换语义,trash 做延迟删除。
- **本章不变量**:I1–I4、I18–I20,以及版本链的存储侧保留保证(I26)。

## 下一章

[05-l3-hnsw.md](05-l3-hnsw.md):全书数学核心——HNSW 从零推导。
