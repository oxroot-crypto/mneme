# 06 L4 检索层:DSL、BM25 与混合融合

> **本章目标**:把"语义检索"(向量)与"字面检索"(BM25 关键词)拼成一条
> 可过滤、可融合、可去重的完整查询管线。
> **前置阅读**:[03 §5](03-l1-memory.md)(过滤 AST)、[04 §5](04-l2-persist.md)(zone map/bloom)、[05 §8](05-l3-hnsw.md)(过滤三档)。
> **本章你将学到**:DSL 文法与解析器 → 查询计划器 → BM25 公式逐项拆解(含手算)→
> RRF/加权融合 → 执行管线 → 去重服务。
>
> **落地状态(2026-09)**:本章已落地于 `src/query/`(解析/展示/JSON 往返、计划器、
> BM25、融合、执行管线)与 `src/memory/analysis/`(内存倒排 / zone map / bloom),
> msec 四区随 `flush` 落盘、`open` 经重排映射重建(设计 04 §5,契约 `FC-PERSIST-POST-008`)。
> 与本章设计的工程口径差异:① 内存引擎只维护一份全局倒排(见 [04 §5.4](04-l2-persist.md)
> 落地注);② 三值语义下 `Not` 不做块级取反(位图取反会把 `Unknown` 误判为命中),
> 交行级残差求值;③ 计划编译仍含 $O(N)$ 的逐行可见性判定(待段句柄重构消除,
> 契约 `FC-QUERY-CPLX-002`);④ `ttl_map` 与段内反向表随 L5 落地。验收:`tests/l4_contracts.rs`。

模块:`query/{parse/{mod,literal}.rs, display.rs, json.rs, iso.rs, plan.rs, zmap.rs, bm25.rs, fusion.rs, exec.rs}`

---

## 1. 过滤 DSL:`parse/{mod,literal}.rs`

### 1.1 文法(EBNF)

```ebnf
expr    = or ;
or      = and { "or" and } ;
and     = not { "and" not } ;
not     = [ "not" ] primary ;
primary = "(" expr ")" | cmp ;
cmp     = path ( "==" | "!=" | ">" | ">=" | "<" | "<=" ) value
        | path "in" "(" value { "," value } ")"
        | path "contains" value                 (* 数组含元素,或字符串含子串 *)
        | path "startswith" value
        | path "endswith" value
        | path "~" string                       (* 通配符:* 任意串,? 单字符 *)
        | "exists" "(" path ")"                  (* 字段存在(可为 null) *)
        | "is_null" "(" path ")" ;               (* 字段存在且为 JSON null *)
path    = ident { "." ident } ;
ident   = ( letter | "_" ) { letter | digit | "_" | "-" } ;
value   = string | number | "true" | "false" | timestamp | duration_expr ;
timestamp    = "ts" string ;                     (* ts"2024-06-01T00:00:00Z" *)
duration_expr = "now" ( "-" | "+" ) duration ;   (* now - 7d *)
duration = number ( "s" | "m" | "h" | "d" | "w" ) ;
```

- **解析器**:递归下降,语法与运算符分派(`parse/mod.rs`)+ 字面量解析(`parse/literal.rs`);优先级 `not > and > or`;
- **路径**:`path` 为 `a.b.c` 形式的点路径(嵌套元数据字段);
- **运算符别名**:`&&` / `||` / `!` 作为 `and` / `or` / `not` 的等价写法被接受
  (01 §6 的 `filter!` 示例即用 `&&`);
- **时间戳字面量**:`ts"…"` 按 ISO 8601 解析为 Unix 毫秒,与 `Val::Ts` 对应
  (时间字段只能与 `ts` 字面量或 `now ± duration` 比较,见 [03 §5.1](03-l1-memory.md));
- 时间量:`now - 7d` 在**查询时**求值为绝对毫秒(相对语义,缓存友好性差但语义正确);
- 类型规则同 [03 §5.1](03-l1-memory.md);**缺失字段采用三值语义**:`Cmp`/`In`/`contains`/
  `startswith`/`endswith`/`~` 对缺失字段求值为 false,而 `Not(false)` 仍为 false(而非 true)——
  即 `not(kind == "x")` **不会**命中没有 `kind` 字段的记录;要查"字段缺失"请用 `exists`。
  这是为 Agent 开放 schema 特意选择的语义(不变量 FC-QUERY-ERR-002);
- 三个入口:`Expr::from_str`(解析)、`Display`(打印)、
  `Meta ↔ Expr`(经 `core::meta` 的 JSON 往返,便于 Agent 框架下发);
- `filter!` 宏 = `Expr::from_str(...).expect(...)` 的舒适封装(运行时仍是解析,宏不会让
  解析发生在编译期);**字面量非法时 panic**——这是文档化的例外,面向"表达式写死在
  代码里"的场景。要处理运行时输入,请用 `Expr::from_str` 返回的 `Result`(不 panic,I7)。

### 1.2 【复杂度】

解析 $O(L)$(L = 表达式长度,单遍);求值 $O(|E|)$/行(见 [03 §5.2](03-l1-memory.md)),
且经 §2 的计划器后,绝大多数行根本不求值。

---

## 2. 查询计划器:`plan.rs` + `zmap.rs`

**目标**:把 AST 编译成"每段一份的执行方案",让数据越少被碰越好。

```text
compile(expr, segment) → Plan {
    block_mask:  每 1024 行块的"可能匹配"位图     ← zone map 求值
    eq_blooms:   等值条件的 bloom 预筛             ← bloom 求值
    residual:    Expr(行级残留谓词,已做选择性重排)
    selectivity: s = popcount(候选位图) / 段活行数   → 传给 HNSW 选档(05 §8)
}
```

- **块剪枝**:对每个合取子条件求块级 min/max(见 [04 §5.2](04-l2-persist.md) 算例);
  `And` = 位图按位与,`Or` = 按位或,`Not` = 取反(注意与全活位图求交,墓碑除外);
- **条件重排**:合取链按"预估选择性"升序排列(等值 + 高选择字段优先),
  短路求值让最便宜的条件先淘汰;
- **残留谓词**:块位图只证明"块内**可能**有匹配",行级仍需精确求值——
  plan 只减少工作量,不改变语义(与逐行求值结果全等,属性测试保证)。

**【复杂度】** 计划:$O(\text{blocks} \times \text{predicates})$ ≈ 千级判断(1M 行);
残留求值只发生在"候选块内的行",通常 ≪ N(经验值:选择性过滤下 < 1% 的行)。

---

## 3. BM25:`bm25.rs`

**BM25(Best Matching 25)** 是经典概率检索模型的具体化:对查询里的每个词,
给"该词出现得多、文档不太长、且该词稀少"的文档高分。它回答的问题与向量检索互补:
向量懂"语义近",BM25 懂"字面命中"——Agent 记忆里专名、代码符号、精确编号
这类**必须逐字匹配**的内容,关键词通道显著更强。

### 3.1 【直觉】三个直觉合一体

1. **词越稀少越值钱**(IDF):"记忆"人人都有,搜它没区分度;"Mneme"只有一个文档有,金贵;
2. **词频有饱和**(TF 饱和):一篇文章"记忆"出现 3 次比 1 次更相关,但 30 次并不比
   3 次好 10 倍——重复刷词不该作弊;
3. **长文档要打折**(长度归一):长文天然词多,比较时按长度惩罚。

### 3.2 【数学】完整公式

对查询 $Q = \{t_1, \dots, t_m\}$ 与文档 $D$:

$$
\text{score}(Q, D) = \sum_{t \in Q} \underbrace{\ln\!\left(\frac{N - df_t + 0.5}{df_t + 0.5} + 1\right)}_{\text{IDF: term rarity}}
\cdot \underbrace{\frac{f(t, D)\,(k_1 + 1)}{f(t, D) + k_1\left(1 - b + b\,\dfrac{|D|}{\text{avgdl}}\right)}}_{\text{TF: saturation + length norm}}
$$

| 符号 | 含义 | 默认 |
|---|---|---|
| $N$ | **查询命名空间内**文档总数(跨全部活跃段全局聚合,见下) | — |
| $df_t$ | 命名空间内含词 $t$ 的文档数 | — |
| $f(t,D)$ | 词 $t$ 在 $D$ 中的出现次数(tf) | — |
| $\|D\|$、avgdl | 文档词数、命名空间平均词数 | — |
| $k_1$ | tf 饱和速度:越大越晚饱和 | 1.2 |
| $b$ | 长度归一强度:0 = 不惩罚长文,1 = 完全按比例 | 0.75 |

**逐项拆解**:

- **IDF 的概率起源**(Robertson–Spärck Jones):"相关文档中出现 $t$ 的概率"与
  "随机文档中出现 $t$ 的概率"之比(优势比,odds ratio)取对数即
  $\ln\frac{P(t|R)}{P(t|\bar R)} \approx \ln\frac{N - df + 0.5}{df + 0.5}$
  (+1 变体保证 $df = N$ 时分数不为负,工程常用);
- **TF 饱和的形状**:$f \to \infty$ 时 TF 项 $\to k_1 + 1$(有界!):
  $\frac{f(k_1+1)}{f + k_1(\cdot)}$,分母中 $f$ 主导 → 分数封顶。
  $k_1$ 控制逼近速度:$k_1 = 0$ 则只看"出现与否",$k_1$ 大则接近线性;
- **长度归一**:$1 - b + b\frac{|D|}{\text{avgdl}}$ 是"等效 tf 折算系数":
  长于平均的文档系数 > 1(tf 更难"达标"),短文档反之。$b=0.75$ 是文献标准值。

**统计范围(命名空间级 + 跨段全局)**:段内混装多个命名空间([07 §5](07-l5-life.md)),
且一个命名空间的数据分散在多个段。若用**段内** $N$/avgdl/df,各段 IDF 尺度不同,跨段归并
与融合会错排。正确做法见 [04 §5.6](04-l2-persist.md) 的**两遍法**:

- 第 1 遍统计:跨所有活跃段累加查询命名空间的 $N$、`total_doc_len` 与每个查询词的全局
  $df_t$,$df_t$ 只计**活行**(排除墓碑与被更新遮蔽的旧版本,避免 IDF 虚高);
- 第 2 遍打分:用同一套全局 $N$/avgdl/$df_t$ 对各段 postings 打分;
- 段内该 NS 无文档时整段跳过;**不变量 I21(BM25 统计一致性)**:N/avgdl/df 按查询命名空间
  跨全部活跃段全局聚合、只计活行,与段数无关,且跨命名空间互不干扰(验收 [14 §3.4](14-testing.md))。

### 3.3 【算例】手算

段内 $N = 1000$,平均文档长 avgdl = 120 词,$k_1 = 1.2, b = 0.75$。
查询词 $t$:$df = 100$。

```
IDF = ln((1000 - 100 + 0.5)/(100 + 0.5) + 1) = ln(8.96 + 1) = ln 9.96 ≈ 2.299

文档 D1: tf=3, |D|=90
  长度系数 = 1 - 0.75 + 0.75*(90/120) = 0.25 + 0.5625 = 0.8125
  TF 项   = 3*2.2 / (3 + 1.2*0.8125) = 6.6 / 3.975 ≈ 1.660
  score   = 2.299 × 1.660 ≈ 3.82

文档 D2: tf=1, |D|=120(平均长)
  系数 = 1 - 0.75 + 0.75*1 = 1.0
  TF 项 = 1*2.2 / (1 + 1.2) = 1.0
  score = 2.299 × 1.0 ≈ 2.30
```

解读:D1 词频是 D2 的 3 倍,得分只高 1.66 倍(**饱和**);D2 长度正好平均,无奖惩。

### 3.4 倒排索引与编码

每个不可变段在 msec 里携带倒排索引:

```text
term_dict: [term → (df, postings_offset, postings_len)]   (段内排序,二分查找)
postings:  [slot_delta varint][tf varint] × df           (SlotId 严格升序,差分编码)
doc 区:    [u32 doc_count] + [u32 ns_id][u32 slot][u32 doc_len] × count   (归一用)
```

> 落盘在词表与 postings 区之间写入 `[u64 postings_total_len]` 显式长度,doc 区起点
> 不靠"各词条声明区间的最大值"推断;重复词条与重复槽位在解码/编码时显式拒绝
> (见 `src/persist/msec/inverted.rs`)。

- **差分 + varint**:SlotId 升序时相邻差多为小整数,varint 平均 1–2 字节
  ([02 §6](02-l0-core.md));整条 postings 空间 ≈ $df \times 3$ 字节量级(经验值);
- **打分复杂度**:对查询的每个词走一遍 postings:

$$T = O\!\left(2\sum_{t \in Q} df_t\right)\ \text{postings accesses (count pass + score pass)}, \qquad S = O(\text{postings})\ \text{(static)}$$

查询只碰"含查询词"的文档——这是 BM25 快的根本;无查询词的文档零成本。
命名空间隔离通过记录体携带的 `ns_id` 判定(记录体带 NsId,[04 §2.2](04-l2-persist.md)):
同一遍扫描同时完成过滤与 $df_t$ 计数,复杂度不变(额外每 posting 一次 `ns_id` 比较)。
- **构建**:flush 时顺带生成(分词 + 排序 + 差分),成本与文档数线性,
  由 compaction 摊销,不在写路径热区;
- **未落段记录**:可变表的**内存增量倒排**同样参与两遍统计与打分([04 §5.4](04-l2-persist.md)),
  因此新写入的 `text` 无需 `flush` 即可被 BM25 检索;合并 postings 时按 `(ns_id, alive)` 过滤,
  仍满足 I21(按查询命名空间全局聚合、只计活行)。

### 3.5 分词(自研,零依赖)

规则:按 Unicode 空白切词 → 小写化 → 去首尾标点;**CJK 连续段做 bigram**
("记忆库" → "记忆","忆库")——bigram 是无词典分词的保底方案,精度对
关键词通道足够;拉丁词按词切。停用词表为内置常量,经 `Tuning::stopwords` 开关(默认开,
见 [16 §2](16-api-reference.md))。未来替换 jieba 级分词器只动 `core::text::tokenize` 一个函数。

---

## 4. 融合:`fusion.rs`

向量通道产出排名 $R_v$(按相似度),BM25 通道产出排名 $R_b$(按 BM25 分)。
两套分数**量纲不同**(余弦 ∈ [-1,1],BM25 无上界),直接加权没有意义。
两种融合:

### 4.1 RRF(Reciprocal Rank Fusion,倒数排名融合)——默认

$$\text{score}(d) = \sum_{i} \frac{1}{k + \text{rank}_i(d)}, \qquad k = 60$$

**【直觉】** 不比分数只比**名次**:名次天然无量纲。$k=60$ 平滑头部差异——
第 1 名(1/61)与第 2 名(1/62)差距微小,避免单一通道独裁。

**【算例】** 三文档两通道(k=60):

```
A: 向量第3, BM25第1 → 1/63 + 1/61 ≈ 0.01587 + 0.01639 = 0.03227
B: 向量第2, BM25第2 → 1/62 + 1/62           = 0.03226   (与 A 几乎平手 ✓)
C: 向量第1, BM25缺席 → 1/61                  = 0.01639   (单通道冠军居下 ✓)
```

**【复杂度】** $O(k)$——只看两个 top-k 名次表,与 N 无关。

### 4.2 加权归一(Weighted)

$$\text{score}(d) = \alpha \cdot \widehat{s_v}(d) + (1-\alpha)\cdot \widehat{s_b}(d), \qquad \widehat{s} = \frac{s^{*} - s^{*}_{\min}}{s^{*}_{\max} - s^{*}_{\min}}$$

- 归一化在**本次查询的结果集内**做(不是全库),否则量纲仍不可比;
- **向量通道方向**:欧氏原始分为距离平方(越小越优),须先取负得到 $s^{*}$ 再归一,否则排序反转;
- 若某通道只有一个结果(`s^{*}_{\max} = s^{*}_{\min}`),该通道归一值取 1,避免除零;
- `Weighted { alpha }`(`alpha ∈ [0,1]`,默认 0.5)供"我就是要向量为主"的场景;融合器整体默认 `Rrf{k:60}`;
- 复杂度 $O(k)$;缺点:对结果集外的高分文档视而不见(两通道 top-k 之外不参与),
  与 RRF 相同——融合都发生在两通道各自 top-k(默认各取 $2k$ 再融合取 $k$,
  减少截断遗憾)。

---

## 5. 执行管线:`exec.rs`

```mermaid
sequenceDiagram
    participant C as 调用方
    participant E as exec(计划器)
    participant V as 向量通道
    participant B as BM25 通道
    participant S as 各段 + 可变表(内存段)

    C->>E: SearchBuilder.execute()
    E->>E: 解析 DSL → Expr;每段 compile → Plan(位图/选择性)
    par 向量通道(并行)
        E->>V: q + 段位图 + s
        V->>S: 三档策略(05 §8)→ 段内 TopK(2k)
    and BM25 通道(并行)
        E->>B: 查询词分词 + 段位图
        B->>S: 倒排打分 → 段内 TopK(2k)
    end
    E->>E: 段间归并 → 双通道 RRF/Weighted 融合
    E->>E: [可选] expand 关系联想 → Scoring 综合打分 → 去重/MMR
    E->>E: [可选] Reranker 回调重排 → top-k
    E->>C: 返回命中列表(物化记录体,快照一致;生成 query_id 供反馈)
```

要点:
- 两通道各取 **2k** 再融合(给融合器留余量),最后取 k;
- **可变表即内存段**:向量通道对其暴力扫描、BM25 通道用内存增量倒排([04 §5.4](04-l2-persist.md)),
  因此新写入无需 `flush` 即可被检索;
- 段内候选位图由 Plan 提供,向量与 BM25 通道**共享**同一份过滤语义——
  过滤是"与"在融合之前,而不是之后;
- `Reranker` 钩子:
  ```rust
  pub struct QueryCtx<'a> { pub text: Option<&'a str>, pub vector: Option<&'a [f32]> }
  pub trait Reranker: Send + Sync {
      fn rerank(&self, q: &QueryCtx, hits: Vec<Hit>) -> Vec<Hit>;
  }
  ```
  `QueryCtx` 同时携带可选的查询文本与查询向量(纯向量检索时文本为 `None`),
  宿主可接 cross-encoder 模型做精排;引擎不内置任何模型;
- **排序全等性**([03 §2.2](03-l1-memory.md))在同一快照内对所有通道生效。

**【算例】一次混合检索的数值走查**:沿用 §4.1 的三文档两通道算例(k=2,k_rrf=60):

```text
候选(过滤后)   向量 sim / 名次     BM25 分 / 名次     RRF 融合分
A              0.91 / 3           3.1 / 1           1/63 + 1/61 = 0.03227
B              0.88 / 2           2.8 / 2           1/62 + 1/62 = 0.03226
C              0.85 / 1           缺席 / —           1/61        = 0.01639
融合取 top-2 → {A, B}(A、B 仅差 1e-5,A 略高;若精确同分则按 rowid 升序)
若开启 Scoring:在 {A,B} 内归一化 ŝ,叠加 recency/importance 等因子后重排;
若开启 MMR:再剔除互相过近者;最后由 Reranker(若注册)定序并返回,同时生成 query_id 供 feedback。
```

这条走查把 §2 的计划器、§4 的融合、[10](10-scoring.md) 的排序与本章 §5 的管线串成一条线:
**过滤先行 → 双通道各自 top-k → RRF 融合 → (可选)扩展/综合打分/去重/重排 → 返回**。

---

## 6. 结果级去重(实现于 L1 `memory::expand`)

写入期去重(`Dedup`,见 [03 §6](03-l1-memory.md))与**结果级去重**是两个独立旋钮;
后者只作用于本次 `execute()` 返回的命中列表:

```rust
pub enum ResultDedup {
    Off,                       // 不去重(默认)
    ById,                      // 同一 RowId 在多段重复命中时只保留分最高者
    Near { threshold: f32 },   // 结果内近似去重:相似度 ≥ threshold 视为重复,保留分高者
}
```

在 [03 §6](03-l1-memory.md) 两级判重(精确 FNV-1a + 近似 top-1)之上,L4 的增量:

- 判重查询可携带**元数据条件**(如仅在同 `kind` 内判重),复用 §2 计划器——
  "同一类记忆"内判重,避免把"事实"和"偏好"误判为重复;
- `Merge` 回调的签名升级为 `(old: &RecordRef<'_>, new: &RecordRef<'_>) -> Option<Record>`,
  返回 None = KeepBoth(`RecordRef` 见 [16 §1.2](16-api-reference.md));
- 全部去重决策发生在**写者锁内**(单写者,天然无竞争),成本 = 一次 top-1 检索
  (L3 后 $O(ef \cdot M_0 \cdot d)$,微秒级)。

---

## 7. 层边界契约(L4 → 上层)

**向上提供**:

1. `SearchBuilder::execute()` 的完整语义(过滤 + 混合 + 融合 + 关系扩展 + 综合打分 +
   去重/多样性 + 重排钩子),管线顺序见 §5;
2. `Expr` 的解析/打印/JSON 往返;`Plan`(内部,含每段位图与选择性);
3. `tokenize()`(分词,公开给需要自建文本索引的宿主);
4. `Reranker` trait、`ResultDedup`、`Scoring`、`Diversity`、`RelationExpand`(与 [10](10-scoring.md) 共用)。

**依赖**:L0(TopK/varint/meta)、L2(段与倒排读取)、L3(带位图的 ANN)。
**不变量**:

- I5 同一快照内,`execute()` 结果 = "候选集内暴力计算 + 标准融合"的结果
  (统计等价于理想实现;ANN 的近似性由 [05 §6.2](05-l3-hnsw.md) 召回门槛约束);
- I6 过滤语义与融合顺序无关(过滤先行,融合只对过滤后候选);
- I7 DSL 解析对任意输入不 panic(模糊测试,见 [14 §5](14-testing.md))。

## 本章小结

- 过滤 DSL 的文法/解析/三值语义;`filter!` 是 `from_str(...).expect(...)` 的舒适封装。
- 计划器用 zone map + bloom 做**块级下推**,残留谓词只作用于候选行。
- BM25 公式逐项拆解;统计按命名空间**跨全部活跃段全局聚合**(两遍法,I21)。
- RRF/加权融合都发生在两通道各自 top-k 上;执行管线顺序固定。
- 结果级去重(`ResultDedup`)与写入期去重(`Dedup`)是两套独立旋钮。
- **本章不变量**:I5(等价性)、I6(过滤先行)、I7(DSL 不 panic)。

## 下一章

[07-l5-life.md](07-l5-life.md):TTL、遗忘曲线与让段数永远有界的 compaction。
