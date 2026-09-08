# 10 术语表、符号表与复杂度速查

> 按主题分组;"详见"列指向完整讲解所在小节,可跳回原文。

---

## 1. 术语表(中英对照)

### 领域概念

| 术语 | 英文 | 一句话定义 | 详见 |
|---|---|---|---|
| 智能体 | Agent | 循环"观察→思考→行动"的 AI 程序 | [00 §1](00-fundamentals.md) |
| 检索增强生成 | RAG (Retrieval-Augmented Generation) | 先检索旧记忆再让 LLM 作答的"开卷考试"模式 | [00 §2](00-fundamentals.md) |
| 嵌入 | embedding | 文本 → 高维数字数组,语义近则空间近 | [00 §3](00-fundamentals.md) |
| 嵌入型数据库 | embedded database | 作为库链接进进程、读写本地文件的数据库(如 SQLite) | [00 §2](00-fundamentals.md) |
| 向量 | vector | 固定长度的浮点数组;长度称维度 | [00 §3](00-fundamentals.md) |
| 维度 | dimension | 向量分量个数,Mneme 上限 65536 | [00 §3](00-fundamentals.md) |
| 近似最近邻 | ANN (Approximate Nearest Neighbor) | 不求绝对最近、以高概率找到近邻的检索策略 | [00 §5.2](00-fundamentals.md) |
| 召回率 | Recall@k | ANN 结果与精确结果的重合比例 | [00 §5.2](00-fundamentals.md) |
| 记忆生命周期 | memory lifecycle | 写入→强化→衰减→遗忘的完整管理 | [07](07-l5-life.md) |
| 命名空间 | Namespace | 记忆的逻辑分区;路径式层级,`(NsId, Key)` 为复合主键 | [07 §5](07-l5-life.md) |
| 记录 | Record | 一条记忆:向量 + 可选 key/text/元数据/TTL/importance | [11 §1.2](11-api-reference.md) |
| 命中 | Hit | 检索结果:RowId、key、score 与记录视图(不含向量) | [11 §1.2](11-api-reference.md) |
| 记录视图 | RecordRef | 存储记录的只读视图(无 score),`get`/`iter` 返回;可取回原始向量 | [11 §1.2](11-api-reference.md) |
| 艾宾浩斯曲线 | Ebbinghaus curve | 记忆保持率随时间指数衰减、回忆可减缓衰减 | [07 §3](07-l5-life.md) |

### 索引与检索

| 术语 | 英文 | 一句话定义 | 详见 |
|---|---|---|---|
| 小世界网络 | small world | 节点度数小但任意两点路径极短的图 | [05 §2](05-l3-hnsw.md) |
| 分层可导航小世界图 | HNSW (Hierarchical Navigable Small World) | 多层近邻图索引;上层稀疏高速、底层全量乡道 | [05](05-l3-hnsw.md) |
| 可导航小世界 | NSW (Navigable Small World) | HNSW 的单层底层图,贪心路由的基础 | [05 §2](05-l3-hnsw.md) |
| 单指令多数据 | SIMD (Single Instruction, Multiple Data) | 一条指令并行处理一排数;AVX2 一次 8 个 f32 | [02 §4](02-l0-core.md) |
| 贪心路由 | greedy routing | 每步走向离目标更近的邻居 | [05 §2.2](05-l3-hnsw.md) |
| 探查宽度 | ef (explore factor) | 搜索时保留的候选队列宽度;召回/延迟旋钮 | [05 §5](05-l3-hnsw.md) |
| 多样性剪枝 | diversity pruning | 选邻居时丢弃被已选者"代表"的冗余候选 | [05 §4.3](05-l3-hnsw.md) |
| 选择性 | selectivity (s) | 过滤命中比例;决定过滤三档策略 | [05 §8](05-l3-hnsw.md) |
| 倒排索引 | inverted index | 词 → 含该词的文档列表 | [06 §3.4](06-l4-query.md) |
| BM25 | Best Matching 25 | 经典概率检索打分:稀缺词×饱和词频×长度归一 | [06 §3](06-l4-query.md) |
| 倒数排名融合 | RRF (Reciprocal Rank Fusion) | 按名次倒数融合多通道,免量纲问题 | [06 §4.1](06-l4-query.md) |
| 重排器 | reranker | 对候选精排的回调钩子(可接 cross-encoder) | [06 §5](06-l4-query.md) |
| 词频 | tf (term frequency) | 词在文档中出现次数 | [06 §3.2](06-l4-query.md) |
| 文档频率 | df (document frequency) | 含该词的文档数 | [06 §3.2](06-l4-query.md) |
| 逆文档频率 | IDF (inverse document frequency) | 词的稀缺度权重 | [06 §3.2](06-l4-query.md) |
| 分词 | tokenization | 文本切成索引词;CJK 用 bigram | [06 §3.5](06-l4-query.md) |

### 存储与数据库

| 术语 | 英文 | 一句话定义 | 详见 |
|---|---|---|---|
| 预写日志 | WAL (Write-Ahead Log) | 只追加的操作流水;先记账后入册 | [00 §6.2](00-fundamentals.md) |
| fsync | fsync | 强制把页缓存刷到物理盘的系统调用 | [00 §6.1](00-fundamentals.md) |
| 组提交 | group commit | 多写者共享一次 fsync | [04 §3.1](04-l2-persist.md) |
| 撕裂写 | torn write | 断电导致的"写到一半" | [00 §6.3](00-fundamentals.md) |
| 循环冗余校验 | CRC (Cyclic Redundancy Check) | 数据指纹;检出意外损坏 | [04 §4](04-l2-persist.md) |
| 段 | segment | 不可变的存储文件三元组 (vsec/msec/hidx) | [04 §1](04-l2-persist.md) |
| 墓碑 | tombstone | "已删除"标记;物理清除留给 compaction | [00 §6.4](00-fundamentals.md) |
| 合并压缩 | compaction | 后台把多段有效数据誊清重写、顺带清理 | [07 §4](07-l5-life.md) |
| 写放大 | write amplification | 实际写盘字节 / 逻辑写入字节 | [00 §6.7](00-fundamentals.md) |
| 多版本并发控制 | MVCC | 读操作看到一致快照,不阻塞写 | [00 §6.6](00-fundamentals.md) |
| 序号 / 水位 | seqno / watermark | 全局单调提交序号 / 快照可见性边界 | [00 §6.6](00-fundamentals.md) |
| 内存映射 | mmap (memory-mapped file) | 把文件映射进地址空间,零拷贝按需加载 | [00 §6.8](00-fundamentals.md) |
| 存活期 | TTL (Time-To-Live) | 记录到期时刻;逻辑过期 + compaction 物理清除 | [07 §1](07-l5-life.md) |
| 分块 min/max 索引 | zone map | 每块的 min/max 摘要,整块剪枝 | [04 §5.2](04-l2-persist.md) |
| 布隆过滤器 | bloom filter | 不漏报、低误报的存在性判定结构 | [04 §5.3](04-l2-persist.md) |
| 双哈希法 | double hashing | 2 个哈希生成 k 个布隆位置 | [04 §5.3](04-l2-persist.md) |
| 变长整数 | varint | 小数占小空间的整数编码 | [02 §6](02-l0-core.md) |
| 快照 | snapshot | 钉住某 ReaderView 的完整只读视图 | [07 §6](07-l5-life.md) |
| 文件系统检查 | fsck | 全量完整性校验与对账 | [07 §7](07-l5-life.md) |
| 记录标识 / 物理槽位 | RowId / SlotId | 全局稳定的记录身份 / 段内物理下标 | [02 §1](02-l0-core.md) |
| 段文件 | vsec / msec / hidx | 向量段 / 元数据段 / HNSW 图段,共享同一 SegmentId | [04 §1](04-l2-persist.md) |
| key 索引 | key index | msec 内 (NsId, key)→SlotId 有序索引,供 `get` | [04 §5.5](04-l2-persist.md) |
| 命名空间注册表 | namespace registry | MANIFEST 内 path↔NsId 映射,供 `list_namespaces` | [04 §2.4](04-l2-persist.md) |
| 批量提交帧 | BatchBegin / BatchCommit | WAL 中包裹整批写入、保证批原子 | [04 §2.3](04-l2-persist.md) |
| FNV-1a | Fowler–Noll–Vo 1a | 精确文本判重用的 64 位非加密哈希 | [03 §6.2](03-l1-memory.md) |
| VNNI | Vector Neural Network Instructions | x86 的 i8 点积指令(`dpbusd`) | [08 §2.3](08-l6-quant.md) |
| 日志结构合并 | LSM (Log-Structured Merge) | 只追加写、后台合并的存储组织方式 | [00 §6.4](00-fundamentals.md) |
| 跳表 | skip list | 多层稀疏链表实现对数查找,是 HNSW 分层思想的同源结构 | [05 §3.1](05-l3-hnsw.md) |
| 交叉编码器 | cross-encoder | 查询与文档一起过模型的精排器,比双塔准但更慢 | [06 §5](06-l4-query.md) |
| 命名空间统计 | ns_stats | msec 内每命名空间的 doc_count/total_doc_len,供 BM25 局部 IDF | [04 §5.6](04-l2-persist.md) |

### 量化([08](08-l6-quant.md))

| 术语 | 英文 | 一句话定义 | 详见 |
|---|---|---|---|
| 标量量化 | scalar quantization | 每分量独立映射到 8 位整数 | [08 §2](08-l6-quant.md) |
| 半精度 | f16 (half precision) | 16 位浮点(1+5+10 位分布) | [08 §3](08-l6-quant.md) |
| 两阶段检索 | two-phase retrieval | 量化粗排 4k 候选 → f32 精排取 k | [08 §4](08-l6-quant.md) |
| 乘积量化 / RaBitQ | PQ (Product Quantization) / RaBitQ | 更强的向量压缩方案;v1 未实现,列为未来 | [08 §3](08-l6-quant.md) |

### API 与运维([11](11-api-reference.md))

| 术语 | 英文 | 一句话定义 | 详见 |
|---|---|---|---|
| 批量原子写 | batch insert | `insert_batch` 整批可见或整批不可见(I15) | [11 §1.2](11-api-reference.md) |
| 快照句柄 | SnapshotHandle | 钉住某 ReaderView(段集 + 可变表快照)的只读视图,时间旅行读(I17) | [11 §1.6](11-api-reference.md) |
| 优雅关闭 | graceful close | `close()` 返回 Ok 即已持久;`Drop` 仅尽力(I16) | [11 §1.7](11-api-reference.md) |
| 时间源 | Clock | 可注入的 Unix 毫秒时钟,保证 TTL/遗忘可测 | [04 §10.2](04-l2-persist.md) |
| 格式版本 | format_version | 文件头版本号;过新则拒绝打开(I18) | [04 §12](04-l2-persist.md) |
| 数据限额 | limits | key/text/meta 等硬上限,超限拒绝 | [11 §8](11-api-reference.md) |
| 时间点恢复 | PITR (point-in-time recovery) | 把 `current` 指回上一 MANIFEST 版本以回滚一个提交点 | [11 §7.2](11-api-reference.md) |
| 结果去重 | ResultDedup | 只作用于单次查询命中列表的去重策略 | [06 §6](06-l4-query.md) |
| 压缩控制 | compaction control | `compact_control()` 的 pause/resume,让后台合并让路 | [11 §1.6](11-api-reference.md) |

---

## 2. 符号表

| 符号 | 含义 | 首现 |
|---|---|---|
| $\mathbf{a}, \mathbf{b}, \mathbf{q}$ | 向量(查询向量记 q) | [00 §3](00-fundamentals.md) |
| $d$ | 向量维度 | [00 §3](00-fundamentals.md) |
| $\|\mathbf{a}\|$ | 向量范数(长度) | [00 §3](00-fundamentals.md) |
| $\mathbf{a}\cdot\mathbf{b}$ | 点积 | [00 §4.1](00-fundamentals.md) |
| $N$ | 数据条数(库/段/文档总数,按上下文) | [00 §5](00-fundamentals.md) |
| $k$ | top-k 的 k | [00 §5](00-fundamentals.md) |
| $M,\ M_0$ | HNSW 上层/第 0 层度数上限(默认 16/32) | [05 §4](05-l3-hnsw.md) |
| $ef,\ ef_c$ | 查询/构建探查宽度(默认 64/200) | [05 §4–§5](05-l3-hnsw.md) |
| $m_L$ | 层级分布系数(默认 1/ln M) | [05 §3.2](05-l3-hnsw.md) |
| $s$ | 过滤选择性 | [05 §8](05-l3-hnsw.md) |
| $m, n, k, p$ | bloom:位数/元素数/哈希数/误判率 | [04 §5.3](04-l2-persist.md) |
| $k_1, b$ | BM25 饱和/长度参数(1.2 / 0.75) | [06 §3.2](06-l4-query.md) |
| $I, T_{1/2}, w$ | importance / 半衰期 / 访问增益权重 | [07 §3](07-l5-life.md) |
| $B, r, T_m$ | 段初始行数 / 分级比 / 同层合并阈值(8k / 4 / 4;正文简记 $T$) | [07 §4.2](07-l5-life.md) |
| $W_{\text{amp}}$ | 写放大系数(≈ $\log_r(N/B)$) | [07 §4.2](07-l5-life.md) |
| $\Delta$ | 量化步长 | [08 §2.2](08-l6-quant.md) |
| $\lambda$ | 指数衰减速率 $\ln 2 / T_{1/2}$ | [07 §3.2](07-l5-life.md) |
| $S$ | 规模/数据量(按上下文:活跃段数见 [04 §5.5](04-l2-persist.md)、合并数据量见 [07 §4.5](07-l5-life.md)) | [04 §5.5](04-l2-persist.md) |
| $L$ | 层数(HNSW 最高层 / compaction 顶层 $\log_r(N/B)$) | [07 §4.2](07-l5-life.md) |
| $W$ | HNSW `SEARCH-LAYER` 的结果集(容量 ef) | [05 §4.1](05-l3-hnsw.md) |

---

## 3. 复杂度速查总表

| 操作 | 时间 | 空间 | 章节 |
|---|---|---|---|
| 点积/余弦/欧氏(单对) | $O(d)$;SIMD ≈ $O(d/8)$ 指令 | $O(1)$ | [02 §3–4](02-l0-core.md) |
| TopK(流式) | $O(N \log k)$ | $O(k)$ | [02 §5](02-l0-core.md) |
| varint 编解码 | $O(\log_{128} x)$ ≤ 10 字节 | 小值 1–2 B | [02 §6](02-l0-core.md) |
| 暴力扫描 | $O(N \cdot d)$(过滤后 $O(N_c \cdot d)$) | $O(N/8)$ 位图 | [03 §4](03-l1-memory.md) |
| CRC-32 | $O(n)$(查表,~GB/s) | 8KB 表 | [04 §4](04-l2-persist.md) |
| WAL 提交(组) | $O(1)$ 内存 + 1 次 fsync/批 | 顺序追加 | [04 §3](04-l2-persist.md) |
| WAL 回放 | $O(\text{未落盘帧数})$ | — | [04 §3.3](04-l2-persist.md) |
| zone map 剪枝 | $O(\lceil N/1024\rceil \times \text{条件数})$ | 16B/块/字段 | [04 §5.2](04-l2-persist.md) |
| bloom 判定 | $O(k) = O(7)$ | $1.44\log_2(1/p)$ bit/元素 | [04 §5.3](04-l2-persist.md) |
| Manifest 提交 | $O(\text{段数})$ 写新文件 | 保留 2 版 | [04 §6](04-l2-persist.md) |
| 恢复(open) | $O(\text{WAL 回放})$ + 段头校验 | mmap 惰性 | [04 §7](04-l2-persist.md) |
| HNSW 构建 | $O(N \cdot d \cdot ef_c \cdot M_0)$ | ≈$(8M+20)$ B/节点 | [05 §4/§6.3](05-l3-hnsw.md) |
| HNSW 查询 | 上界 $O(d \cdot ef \cdot M_0)$;实测 ≈ (2–5)·ef 次点积 | — | [05 §6.1](05-l3-hnsw.md) |
| 层级分布 | $P(\ge l) = (1/M)^l$;层高 $O(\log_M N)$ | — | [05 §3.2](05-l3-hnsw.md) |
| DSL 解析 | $O(L)$ 单遍 | $O(\|E\|)$ | [06 §1](06-l4-query.md) |
| BM25 打分 | $O(\sum_{t \in Q} df_t)$ 堆操作 | 静态倒排 | [06 §3.4](06-l4-query.md) |
| RRF/加权融合 | $O(k)$ | $O(k)$ | [06 §4](06-l4-query.md) |
| TTL 逻辑过期 | $O(\text{块数})$(块级 min 剪枝) | 8B/块 | [07 §1](07-l5-life.md) |
| retain 扫描 | $O(N_{\text{候选}})$ | — | [07 §3.4](07-l5-life.md) |
| compaction 单轮 | $O(S \cdot d \cdot ef_c \cdot M_0)$(建图主导) | 峰值 +$O(S)$ | [07 §4.5](07-l5-life.md) |
| compaction 摊还 | 每字节重写 ≈ $\log_r(N/B)$ ≤ ~7 次 | 段数 $O(\log_r N)$ | [07 §4.2](07-l5-life.md) |
| i8 量化点积 | 带宽 ÷4;VNNI 再 ~4× 指令 | $d$ B/行 | [08 §2](08-l6-quant.md) |
| 单点写(insert) | $O(1)$ 内存 + WAL 追加;fsync 按策略 | $O(d)$ | [04 §3](04-l2-persist.md) |
| 单点读(get key) | $O(\log n)$(key 索引二分)+ 一次记录读 | — | [04 §5.5](04-l2-persist.md) |
| 单点读(get_by_rowid) | $O(\log n)$(slot 表二分) | — | [04 §2.2](04-l2-persist.md) |
| delete / touch | $O(\log n)$ 定位 + 墓碑/统计更新 | — | [03 §2.3](03-l1-memory.md) |
| iter(filter) | $O(N_c)$($N_c$ = 命中行) | 流式 | [03 §2.3](03-l1-memory.md) |
| snapshot | $O(1)$(clone Arc 视图) | 按引用 | [07 §6](07-l5-life.md) |
| backup_to | 同盘 $O(\text{文件数})$;跨盘 $O(\text{数据量})$ | 目标目录 | [07 §6](07-l5-life.md) |
| check(fsck) | $O(\text{全量字节})$ CRC + 对账 | — | [07 §7](07-l5-life.md) |

**性能承诺汇总**:Recall@10 ≥ 0.95(ef=128);1M×1536 量化后 P99 < 10ms;
批量插入 ≥ 50k 向量/秒;冷启动 < 1s;活跃段数有界。验收方法见 [09](09-testing.md)。

## 下一章

[11-api-reference.md](11-api-reference.md):完整公开 API、配置与运维参考。
