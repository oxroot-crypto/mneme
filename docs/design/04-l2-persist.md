# 04 L2 持久层:WAL、段文件与崩溃恢复

> **本章目标**:让"重启不丢数据"成立:定义全部文件格式的**字节级布局**,
> 讲透 WAL 提交/回放、CRC 撕裂写检测、Manifest 原子替换与恢复流程。
> **前置阅读**:[00 §6](00-fundamentals.md)(WAL/CRC/MVCC/mmap 科普)、[03](03-l1-memory.md)。
> **本章你将学到**:目录与文件布局 → 四种文件的字节图 → WAL 协议 → CRC 数学 →
> zone map 与 Bloom filter 完整推导 → Manifest 原子性(Windows 专项) → 恢复流程。

模块:`persist/{wal.rs, vsec.rs, msec.rs, manifest.rs, recover.rs, flush.rs, source.rs, trash.rs}`

---

## 1. 目录布局

```text
agent_memory/
├── current              # 指针文件:内容 = 当前 MANIFEST 版本号(如 "42")
├── MANIFEST.000041      # 旧版本 manifest(write-once,保留最近 2 个)
├── MANIFEST.000042      # 当前版本 manifest
├── wal/
│   ├── wal_000001.log   # 预写日志(可多个,顺序编号)
│   └── wal_000002.log
├── segments/
│   ├── seg_000007.vsec  # 向量段(vectors + norm + 删除位图)
│   ├── seg_000007.msec  # 元数据段(记录体 + slot 表 + key 索引 + zone map + bloom + 倒排 + ns 统计)
│   └── seg_000007.hidx  # HNSW 图段(L3 起出现;L2 阶段不存在)
└── trash/               # 待物理删除的文件(见 §9)
```

**段(segment)** 是不可变文件三元组:`(vsec, msec[, hidx])` 共享同一 `SegmentId`。
"不可变"是整个存储设计的锚点:只追加新段、只改 Manifest 指针,
读路径因此只需短暂读锁换一次视图引用、扫描全程无锁(§8)。可变性由两个例外承担:WAL 阶段的内存可变表(L2 期间)、
后台 compaction 的重写(L5)。

---

## 2. 文件字节布局

约定:整数**小端**(LE);变长字段自带长度前缀;每个文件头部与数据尾部各一个 CRC-32。
头内定长字段按 8 字节自然对齐摆放,便于 mmap 后零拷贝读取。

> **可移植性**:文件格式固定小端。小端平台上 mmap 后可直接零拷贝读取;大端平台需
> 逐字段字节交换(或整体走 `FileSource` 解码路径),正确性不变,但不在 v1 的性能承诺内。
> 跨架构迁移 = 复制目录即可,格式本身与架构无关。

### 2.1 vsec(向量段)

```text
偏移   字段                          大小
0      magic "VSC1"                  4
4      format_version                u16
6      header_len                    u16
8      dimension                     u32
12     metric                        u8     (0=Cosine 1=Dot 2=Euclidean)
13     quant                         u8     (0=F32;其余值 L6 定义)
14     norm_col                      u8     (1 = 附带 norm 列)
15     reserved                      u8
16     row_count                     u64
24     created_unix_ms               i64
32     header_crc32                  u32    (覆盖 [0,32))
36     padding to 64B
--- 数据区(每行定长,32B 对齐) --------------------------------------
       vec[0..count]: f32 × dim      (LE,补齐到 32B 对齐)
       norm[0..count]: f32 × count   (norm_col=1 时)
       del_bitmap: 每 1024 行一块,块内 16 个 u64 字(共 1024 bit;1 = 已删除/墓碑)
--- 尾部 ------------------------------------------------------------
       payload_crc32                 u32    (覆盖整个数据区;可选懒校验)
```

- **32B 对齐**:[02 §4](02-l0-core.md) 的 AVX2 内核要求;`dim × 4` 不是 32 的倍数时
  每行补齐(pad 区不参与计算);
- **删除位图分块**:查询时按块取"该块是否全活"的单 bit 摘要,整块全活零位图读取;
- 物理位置即 `SlotId = 块号 × 1024 + 块内偏移`,位图天然支持 O(1) 判定;
  全局记录标识 `RowId` 由 msec 的 slot 表给出(见 §2.2),用于 `get_by_rowid` 定位。

### 2.2 msec(元数据段)

```text
0      magic "MSC1"                  4
4      format_version                u16
6      header_len                    u16
8      row_count                     u64
16     field_dict_offset             u64    → 字段字典(见下)
24     zmap_offset / zmap_len        2×u64  → zone maps
40     bloom_offset / bloom_len      2×u64  → bloom 组
56     slot_table_offset             u64    → RowId→(SlotId, doc_offset) 有序数组
64     key_index_offset / key_index_len  2×u64 → (NsId, key)→SlotId 索引(§5.5)
80     inv_offset / inv_len          2×u64  → 倒排索引(词表/postings/doc_len,§5.4)
96     ns_stats_offset / ns_stats_len 2×u64 → 命名空间级统计(§5.6)
112    header_crc32                  u32
--- 数据区 ----------------------------------------------------------
       doc_region: 连续的记录体(变长,记录体格式见下;段内顺序即 SlotId 顺序)
       slot_table: (RowId u64, SlotId u32, doc_offset u64) 有序数组 → 按 RowId 二分定位
        key_index:  按 (NsId, key) 排序的 [(NsId u32, key len+bytes, SlotId u32, doc_offset u64)] → get(key) O(log n),直接定位记录体
       field_dict:  [(u16 field_id, len, name bytes)...]  上限 16 个索引字段(默认,见 §5.1)
        zone_maps:   每 1024 行一块 × 每个索引字段: (min, max, has_null)
        (数值/时间字段用 f64/i64 存储;created_at 恒定索引)
       ttl_map:     每 1024 行一块一个 min(expires_at)(无 TTL 行记 +∞)→ TTL 整块剪枝(§5.2)
       blooms:      每个高基数字符串字段(含 key)一个 bloom(参数见 §5.3)
       inverted:    倒排索引 = term_dict + postings + doc_len
                    (编码与打分见 [06 §3.4](06-l4-query.md);无文本记录时为空)
       ns_stats:    每命名空间一行 (NsId u32, doc_count u64, total_doc_len u64)
                    → BM25 的 N/avgdl 按命名空间统计(§5.6)
--- 尾部 ------------------------------------------------------------
       payload_crc32                 u32
```

**记录体(entry)格式**(doc_region 内,长度前缀):

```text
[u32 total_len]
[u64 rowid][u64 seqno][u32 ns_id]
[u8 flags]                        # bit0=有key bit1=有text bit2=有ttl bit3=有importance bit4=有access
[key: len+bytes(可选)]
[text: len+bytes(可选)]
[meta: u32 json_len + serde_json 字节]
[i64 created_at_ms][i64 expires_at_ms(可选)][f32 importance(可选)]
[i64 last_access_ms][u32 access_count](可选,bit4;缺省视为 0/0)
```

`ns_id` 逐条存储,因为同一段内混合多个命名空间([07 §5](07-l5-life.md) 的单库前缀聚合),
记录体的命名空间不可从段位置推断;它同时是 [11 §8.1](11-api-reference.md) 中保留字段 `__ns` 的
持久化来源。

设计取舍:记录体是**变长 blob 连续排放**——`get(key)` 走 key_index 二分
$O(\log n)$ 一次读;顺序重放/compaction 顺序读,对页缓存与 SSD 都友好。
段内物理槽位 `SlotId` 即记录体在 doc_region 中的顺序号,`slot_table` 把它与
全局稳定的 `RowId` 关联起来。

### 2.3 WAL 帧格式

```text
文件头(偏移 0 起,字段按对齐摆放,补齐至 32B):
0   magic "WAL1"(4) | 4 u16 ver | 6 u32 dim | 10 u8 metric | 11 u8 reserved | 12 crc32 | 16..32 保留
之后为帧序列,每帧:
[ u32 crc32 ][ u32 payload_len ][ u8 type ][ payload ]
crc32 覆盖 [payload_len, type, payload](即本帧除自身 crc 外的全部字节)
type: 1=Insert 2=Delete 3=Touch 4=Checkpoint 5=BatchBegin 6=BatchCommit
Insert     = 记录体(同 msec entry 格式,含 NsId)
Delete     = [NsId u32][key len+bytes]
Touch      = [NsId u32][key len+bytes][i64 at_ms][f32 importance_delta]
BatchBegin = [u32 record_count]                  # 后续连续 Insert 帧属于同一原子批
BatchCommit= [u32 record_count][u32 batch_crc]   # 批提交标记;缺此帧则整批丢弃
Checkpoint = [u64 watermark_seqno]
```

帧头三字段(`crc` + `len` + `type`)是撕裂写检测的关键:恢复时若读不满 `len`、
或 CRC 不符 → 该帧及其后全部丢弃(WAL 是**只追加**的,尾部之后不可能是有效数据)。
`BatchBegin`/`BatchCommit` 让 `insert_batch` 的整批写入在崩溃后要么全部重放、
要么整批丢弃(不变量 I15,回放规则见 §3.3)。

### 2.4 MANIFEST

```text
偏移   字段                              大小    说明
0      magic "MNF1"                     4
4      format_version                    u16
6      header_len                        u16     固定头部总长
8      header_crc32                      u32     覆盖除自身外的全部头部字段
12     dimension                         u32     建库维度(空库也可回读;打开时校验)
16     metric                            u8      0=Cosine 1=Dot 2=Euclidean
17     reserved                          7
24     manifest_version                  u64
32     watermark_seqno                   u64
40     next_rowid                        u64     全局记录标识水位(永不复用)
48     next_segment_id                   u32
52     next_ns_id                        u32     命名空间编号分配水位(永不复用)
56     active_count                      u32
60     ns_count                          u32
--- 变长区(紧随固定头部) ----------------------------------------------
[ NsEntry ] × ns_count:                  # 命名空间注册表(path ↔ NsId)
    u32 ns_id, u32 path_len, path bytes(UTF-8)
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
打开时调用方显式传入的维度/度量与此比对,不一致即拒绝([11 §3](11-api-reference.md))。

MANIFEST 是**命名空间路径的唯一事实来源**(`NsEntry` 表);删除命名空间时从表中移除
该路径,但 `next_ns_id` 只增不减,保证 NsId 永不复用([07 §5](07-l5-life.md))。
HNSW 入口是**每段一个**(与 [05 §9–§10](05-l3-hnsw.md) 的"每段独立图"一致),不存在全局单入口。

---

## 3. WAL 协议

### 3.1 提交流程(写路径)

```text
1. 取写锁(全局串行) → 分配 seqno
2. 构帧追加进 WalWriter 的缓冲区:单条写 = Insert/Delete/Touch;
   `insert_batch` = BatchBegin + N×Insert + BatchCommit(整批一次组提交)
3. 按 FsyncPolicy 等待持久确认:
     Always      → 每帧 write + fsync 后返回
     Batched(d)  → 写线程每 d 毫秒统一 write+fsync;提交者等待"上一次 fsync 完成"事件
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
- **Checkpoint(type 4)**:内存可变表已全部 flush 成不可变段、且该段已在 MANIFEST
  生效后写入,记录 `watermark_seqno`;此后 `seqno ≤ watermark` 的 WAL 文件整体删除。
- **L2 阶段兜底**(还没有 compaction):WAL 总量超限(默认 256MB)即触发一次
  "全量快照 flush"(把可变表整体写成一个段),保证 WAL 有界。L5 之后该兜底由
  正规 compaction 取代。

### 3.3 回放(恢复路径的一部分,见 §7)

按序读帧 → 校验 `len`/`crc` → 应用 `type ∈ {Insert, Delete, Touch}` 到内存可变表
→ 遇坏帧即停并截断文件。**只回放 `seqno > manifest.watermark` 的帧**——
之前的已经在段里了。

**批原子回放**:遇到 `BatchBegin` 时把后续 Insert 帧先暂存,直到读到配对的
`BatchCommit`(校验 `batch_crc` 与条数)才一次性应用;若在提交帧前遇坏帧/文件结束,
整批丢弃——由此保证 I15(验收见 [09 §2.1](09-testing.md))。

### 3.4 【复杂度】

| 操作 | 时间 | 磁盘 |
|---|---|---|
| 单条提交(Batched,组满) | $O(1)$ 内存追加 + 等待 | 顺序写 |
| 单条提交(Always) | 1 次 fsync ≈ 0.1–10ms(经验值,盘型决定) | 顺序写 |
| 组提交 N 条/批 | $O(N)$ 追加 + **1 次** fsync | 顺序写 |
| 回放 | $O(\text{未落盘记录数})$ | 顺序读 |

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

(纯文本:`R = (M 左移 32 位) mod G`,R 即 32 位 CRC)

传输 `M·x³² + R`(补余数使整体被 G 整除);接收端重除,G(x) 首尾项保证:
若余数 ≠ 0 → 必有错。**检错能力**(代数可证):所有长度 ≤ 32 bit 的
**突发错误**(burst,连续错误段)全部可检出;所有奇数个 bit 错误可检出
(G 含因子 $(x+1)$);1 bit 错误 100% 检出。

### 4.3 【工程】在 Mneme 里的四处岗哨

1. WAL 每帧:防撕裂写(读不满/CRC 不符 → 截断);
2. 文件头:防元数据损坏;
3. vsec/msec 尾部 payload CRC:**启动时可配校验**(默认只校验头部,全量校验走 `db.check()`,
   1GB 段全量 CRC ≈ 1s 量级,启动时间不为其买单);
4. MANIFEST payload:Manifest 坏 → 回退旧版本(§6)。

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

$$P(\text{bit 为 }0) = \left(1 - \frac{1}{m}\right)^{kn} \approx e^{-kn/m}$$

误判率(查一个未插入的元素,k 个位置全被别人占满):

$$p = \left(1 - e^{-kn/m}\right)^{k}$$

(纯文本:`p = (1 - e^(-(k*n)/m))^k`)

对 k 求导取最优 $k^{*} = \dfrac{m}{n}\ln 2$,代回得空间-误判率关系:

$$\frac{m}{n} = \frac{\log_2 (1/p)}{\ln 2} \approx 1.44 \, \log_2(1/p) \ \text{bit/元素}$$

(纯文本:`bits_per_element ≈ 1.44 * log2(1/p)`)

**【算例】** n = 10 万,p = 1%:$m/n = 1.44 \times \log_2 100 \approx 9.59$ bit/元素
→ $m \approx 9.6 \times 10^5$ bit ≈ **120 KB**,$k^{*} = 9.59 \times 0.693 \approx 6.6 \Rightarrow k = 7$。
代入验证:$p = (1-e^{-7\times10^5/9.59\times10^5})^7 = (1-e^{-0.73})^7 \approx 0.010$ ✓

**【工程】** 哈希用**双哈希法**(Kirsch–Mitzenmacher):只需两个 64 位哈希 $h_1, h_2$
(取自 crc32 组合),第 i 个位置 $h_i(x) = h_1(x) + i \cdot h_2(x) \bmod m$——
k 次哈希的成本变成 2 次哈希 + k 次乘加。误报的后果只是"多评估几行",**安全性无害**。
Mneme 在 msec 每段每字符串字段放一个 bloom(fpp 1%,默认),给等值过滤下推用;
范围过滤走 zone map,两者互补。

### 5.4 倒排索引(为 BM25 供数据)

有 `text` 的记录在 flush 时顺带构建倒排(词表 → postings 差分序列 → doc_len 列),
编码细节与 BM25 打分见 [06 §3.4](06-l4-query.md)。此处只定两条规则:

1. 倒排是 **msec 的一部分**(同一 CRC 保护、同生同灭),不是独立文件——
   保证"向量、元数据、文本索引"三者永远一致(同一 Manifest 版本 = 同一批物理行);
2. 段内无文本记录时该区域为空(`inv_len = 0`),零开销。

### 5.5 key 索引(为 `get(key)` 与去重供路)

每段在 msec 内维护按 `(NsId, key)` 排序的索引 `[(NsId, key, SlotId)]`(§2.2):
`get(key)` 二分 $O(\log n)$ 命中 `(SlotId, doc_offset)`,直接定位记录体。
key 同时进入该字符串字段的 bloom(§5.3),不存在的 key 先被 bloom 挡掉,
避免无谓的索引查找。删除在 compaction 时物理剔除;墓碑槽位由 `dead` 位图在读取时过滤。

**跨段解析与墓碑覆盖**(`get`/`delete`/`touch`/upsert 的共同规则,必须逐段合并):

```text
一次 get(key) 需要看两个来源:
  a) 各活跃段: 对每段 key_index 二分,命中则得到一个候选 (RowId, seqno, SlotId);
  b) 可变表:   内存 key_index + 本段 pending 的墓碑/更新(尚未 flush 成段)。
候选按以下优先级选出唯一可见记录:
  1. 丢弃墓碑命中的候选(段内 dead 位图 或 可变表墓碑集);
  2. 剩余候选取 seqno 最大者(同 key 多次 upsert = 多版本,新版本胜出);
  3. seqno 相同不可能(全局单调分配),故无并列。
```

- upsert 分配新 `RowId`、给旧行打墓碑([03 §2.1](03-l1-memory.md)),新旧可能落在不同段;
  上述规则保证 `get(key)` 总返回最新活版本,直到 compaction 把旧版本物理清除;
- `delete(key)`/`touch(key)` 写一条按 `(NsId, key)` 的墓碑/更新记录进可变表与 WAL
  (§2.3 的 Delete/Touch 帧),它在读取时**遮蔽所有更早的段内版本**;
  compaction 时该遮蔽关系被物化(旧行剔除,存活行写新段);
- 无 key 的记录不参与本规则,只能经 `RowId`(`get_by_rowid`)或过滤器访问
  ([11 §1.3](11-api-reference.md));
- 代价:$O(S)$ 次段内二分($S$ = 活跃段数 $\le O(\log N)$,[07 §8](07-l5-life.md)),
  bloom 先行否定可把绝大多数未命中 key 降到近零成本。

### 5.6 命名空间级统计(为 BM25 供正确的 N/avgdl)

段内混装多个命名空间([07 §5](07-l5-life.md)),而 BM25 的 IDF 依赖"文档集"范围。
若直接用段级 $N$/avgdl,其他命名空间的文档会稀释 IDF。故每段在 msec 内额外存
`ns_stats: (NsId, doc_count, total_doc_len)`(§2.2):

- BM25 的 $N$ 取**查询命名空间**的 `doc_count`,avgdl 取 `total_doc_len / doc_count`;
- $df_t$ 在遍历该词 postings 时按 `ns_id` 统计(记录体带 NsId,§2.2),与过滤同一次扫描完成;
- 段内该命名空间无文档时 $N = 0$,该段 BM25 通道直接跳过;
- 统计随段生成时一次算好、不可变,查询时零额外结构;成本是每段每命名空间 20 字节,
  命名空间数远小于段内行数时可忽略。

---

## 6. MANIFEST 原子性:write-once + 指针(Windows 专项)

**问题**:Manifest 更新必须原子。Unix 上 `rename(tmp, dest)` 可原子覆盖目标;
Windows 的 std `rename` 虽对应 `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`、对已存在的
普通文件也能覆盖,但替换语义并不可靠——目标被其他句柄打开(共享冲突)或带只读属性时
会失败,而 `ReplaceFileW` 需要额外 unsafe/依赖,违反依赖白名单。为保证任意一步崩溃后
仍有一致可用的 Manifest,干脆不依赖"覆盖"这条路:

**方案:write-once 版本文件 + 指针**:

```text
提交 v42:  ① 写 MANIFEST.000042.tmp → fsync
           ② rename → MANIFEST.000042      (目标不存在,rename 原子且平台通用)
           ③ fsync 目录
           ④ 写 current.tmp("42") → rename → current
读路径:    读 current → 打开 MANIFEST.<v> → 校验 CRC
           失败 → 扫描目录所有 MANIFEST.*,取"最大的、CRC 合法的"版本
保留策略:  始终保留最近 2 个版本 → 任意一步崩溃都至少有一个完整可用 manifest
```

每次提交 = 一个新文件,**永不原地覆盖**;这正是 [00 §6.6](00-fundamentals.md)
"读端无锁"的基础:读者打开一个 Manifest 版本后,其引用的段文件都不可变,
整个读路径与写路径零共享可变状态。

---

## 7. 恢复流程:`recover.rs`

```text
open(dir):
 1. 读 current → 定位 MANIFEST.<v>;CRC 失败 → 扫描取最大合法版本;全坏 → 报 Corrupted
 2. 校验各段头部 CRC(全量 CRC 视配置);头部损坏段 → 移入 trash/ 并从视图剔除
    (可配 fail-fast 模式:直接报错拒绝启动)
 3. 打开 WAL 文件(按编号序),重放 seqno > watermark 的帧到内存可变表;
    遇撕裂帧 → 截断该文件尾部
 4. 构建 ReaderView{ manifest 版本, 可变表 } → 对外服务
不变量:恢复后的状态 = "已 fsync 确认的全部操作" 的重放结果(可多,不可错;
       Batched 策略下多出的只能是被 OS 已刷盘但确认事件未送达的帧,重放是幂等的)
```

---

## 8. 读路径:ReaderView 与快照

```text
Reader = RwLock<Arc<ReaderView>>   ← [01 §2.1] 的具体形态
ReaderView(不可变):
  segments: Arc<[SegmentHandle]>     # 每段持有 source(Arc<mmap 或 File>)
  mutable: Arc<MutableSnapshot>      # 取视图时可变表的不可变快照(WAL 已应用部分)
  watermark: SeqNo
  tombstones: 每段一个 Arc<BitSet>   # 删除在视图上表现为替换位图+新视图
读: 拿读锁 clone Arc → 放锁 → 段扫描 + 可变表扫描 → 全局归并
删除/插入: 修改可变表 → flush 时生成新段 → 提交新 Manifest → 写锁内换 Arc
```

旧视图因 `Arc` 仍被读者持有而存活——**读者永远看到一致的过去**,
这正是 MVCC 水位的工程形态。`SnapshotHandle` 即"钉住一个 `ReaderView`":
它同时包含当时的段集**与可变表快照**,因此能看到取快照前所有已确认写入,
无需先 `flush`([07 §6](07-l5-life.md))。

---

## 9. trash:Windows 上的延迟删除

Windows 不允许删除被 mmap/句柄打开的文件。方案:

```text
提交新 Manifest 后: 旧段文件 rename → trash/<name>(rename 打开中的文件是允许的)
登记待删表 {name → 引用计数};最后一个读者 Drop 时(或下次 open 时)尝试删除;
删不掉(仍被旧视图引用)→ 留在 trash,下次启动再试。
```

磁盘占用上界:trash ≈ 一轮 compaction/flush 的增量(经验值:总量的 5–15%),
启动时清理保证不累积。

---

## 10. 测试注入点:`FsyncHook` 与 `Clock`

### 10.1 崩溃注入:`FsyncHook`

```rust
pub trait FsyncHook: Send + Sync {
    /// 在每次 write/fsync/rename 前调用;可注入故障(丢写/翻转字节/截断/崩溃)
    fn before(&self, action: IoAction) -> std::io::Result<()>;
}
```

仅测试 builder 暴露。用法见 [09 §2](09-testing.md):在随机点"杀死"进程,
断言恢复后状态 = 已确认操作前缀。

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

`MmapSource`(feature `mmap`,memmap2)与 `FileSource`(std,`seek+read`)。
索引层与恢复层只依赖此 trait——mmap 是**优化**而非功能依赖,任何平台不支持时可整体退化为
`FileSource`(读吞吐降,正确性不变)。

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
  主版本 ≤ 本库支持上界即可读;次版本更高的文件按"忽略未知可选字段"处理
  (若未知字段带长度前缀则跳过,否则该次版本不得跨读,由主版本机制拒绝);
- **升级不阻塞**:打开旧库无需等待全量重写,读路径兼容旧段,新写入与 compaction
  产生新版本段,库内可短暂**混合版本**;全部段升级完成后 MANIFEST 只含新版本;
- **不支持降级**:新版库不能被旧版 Mneme 打开——升级前先备份;
- 迁移在后台 compaction 中随段重写推进,无需人工干预;`db.stats()` 的 `compaction`
  字段可见后台任务是否在运行。

## 13. 失败模式与处置

| 故障 | 触发 | 引擎行为 | 调用方处置 |
|---|---|---|---|
| 磁盘满(ENOSPC) | write/fsync 返回错误 | 写入返回 `Io`;compaction **暂停**而非损坏;WAL 不推进 | 清理 `trash/` 或扩容,重试 `flush()` |
| 只读文件系统 | open 后首次写 | 打开成功(只读),写返回 `Io` | 换可写目录 |
| 目录被占用 | 第二个实例打开 | `Busy` | 确保单进程独占([11 §3](11-api-reference.md)) |
| 全部 MANIFEST 损坏 | 扫描 `MANIFEST.*` 无合法版本 | `Corrupted` | 从备份恢复([11 §7](11-api-reference.md)) |
| 个别段头损坏 | 打开时校验失败 | 移入 `trash/` 并从视图剔除(可配 fail-fast) | `db.check()` 复核;必要时从备份补段 |
| WAL 未知帧类型 | 回放遇到 `type` 不在定义内 | **停止回放并报错**(不静默跳过) | 升级库版本;切勿手工改 WAL |
| mmap 失败 | 平台/文件系统不支持 | 自动退化为 `FileSource` | 无(功能不变,吞吐下降) |
| 时钟回拨 | `Clock` 返回变小 | 以历史最大水位钳制(本章 §10.2) | 无 |
| 崩溃残留锁文件 | 上次进程未正常退出 | 打开时校验锁内 PID/时间戳,陈旧则接管([11 §3](11-api-reference.md)) | 无(自动接管) |

**原则**:任何失败都不得静默返回错误数据(I2);宁可拒绝启动或返回 `Err`,
也不做"尽力而为"的猜测。用户侧完整 runbook 见 [11 §7](11-api-reference.md)。

## 14. 层边界契约(L2 → 上层)

**向上提供**:

1. `Database: VectorStore`([03 §8](03-l1-memory.md) trait 的持久实现);
2. 段格式编解码(`vsec/msec/wal/manifest` 的 read/write/replay,字节布局本章 §2);
3. `SegmentSource` 抽象、`FsyncHook` / `Clock` 注入点、`trash` 管理;
4. 事务语义:单次 `insert_batch` 原子(要么整批进 WAL,要么整批不出现);
   flush + Manifest 提交 = 一致性点;
5. 格式版本校验与在线迁移(§12)、失败模式处置约定(§13)。

**不变量**(任何上层、任何测试可依赖):

- I1 已确认(返回 Ok)的写入,重启后可见;未确认写入,重启后**要么完整可见要么不存在**;
- I2 任意文件任意 bit 损坏,可被检出(或拒绝启动),绝不静默返回错误数据;
- I3 活跃段集合任意时刻 = 某 MANIFEST 版本所列集合(读端无锁的前提);
- I4 WAL 总量有界(§3.2 兜底),段文件只增不改(write-once);
- I18 只接受 `major(format_version) ≤ major(max_supported)` 的文件,主版本更高的拒绝打开(§12);

## 下一章

[05-l3-hnsw.md](05-l3-hnsw.md):全书数学核心——HNSW 从零推导。
