# 13 记忆模式手册(Agent 开发者 Cookbook)

> **本章目标**:把前面各章的机制组合成**可直接照抄的 Agent 记忆配方**。
> **前置阅读**:[01 §6](01-overview.md)(API 速览)、[16](16-api-reference.md)(参考手册)、[09](09-memory-model.md)/[10](10-scoring.md)(记忆模型与排序)。
> **本章你将学到**:命名空间布局 → 会话/长期记忆配方 → 去重与沉淀 → 安全遗忘 →
> 混合检索与反馈 → 运维与部署。
> 每个配方都标注了它用到的机制与默认取舍。
>
> 配方中的 `filter!` / `json!` 由 Mneme 导出;`embed(...)`(宿主嵌入模型)、`uuid`、
> `now_ms()` 为宿主侧示意代码,不是库 API。

---

## 0. 端到端最小示例(先跑通,再看配方)

一个完整的"建库 → 写入 → 混合检索 → 反馈 → 关闭"流程。`embed(...)` 是宿主嵌入模型,
不是库 API(见章首说明)。

> **可直接运行的版本**:`examples/memory`(交互式 REPL,经 OpenAI 协议接真实嵌入 API,
> 内置 mock 端点的离线路径用例);配置与命令见 [README](../../README.md) 的「端到端示例」。

```rust
use mneme::{filter, json, Diversity, Feedback, FsyncPolicy, Metric, Mneme, Record, Scoring};
use std::time::Duration;

fn main() -> mneme::Result<()> {
    // 1. 建库(目录已存在则从 MANIFEST 读回维度/度量)
    let db = Mneme::builder()
        .path("./agent_memory")
        .dimension(1536)
        .metric(Metric::Cosine)
        .fsync(FsyncPolicy::Batched(Duration::from_millis(20)))
        .build()?;
    let ns = db.namespace("agent-42/profile");

    // 2. 记住两条信息(向量由宿主嵌入模型产生)
    ns.insert(
        Record::new(embed("用户喜欢深色模式")?)
            .key("pref.theme")
            .text("用户喜欢深色模式")
            .metadata(json!({"kind": "preference"}))
            .importance(0.8),
    )?;
    ns.insert(
        Record::new(embed("用户用 macOS")?)
            .key("pref.os")
            .text("用户用 macOS")
            .metadata(json!({"kind": "fact"}))
            .importance(0.6),
    )?;

    // 3. 混合检索:向量 + BM25 + 过滤 + 综合打分 + MMR 多样性
    let q = embed("他喜欢什么界面风格?")?;
    let hits = ns.search()
        .vector(&q)
        .text("界面风格")
        .filter(filter!(r#"kind == "preference""#))
        .score(Scoring { w_recency: 0.2, w_importance: 0.3, ..Scoring::default() })
        .diversify(Diversity::Mmr { lambda: 0.7 })
        .top_k(10)
        .execute()?;
    for h in &hits {
        println!("{:?} {:?} score={:.3}", h.rowid, h.key, h.score);
    }

    // 4. 反馈闭环(以 (rowid, query_id) 幂等)
    if let Some(h) = hits.first() {
        ns.feedback(h.rowid, Feedback::Used, h.query_id)?;
    }

    // 5. 优雅关闭:返回 Ok 即所有已确认写入已持久
    db.close()?;
    Ok(())
}
```

**关键点**:

- 维度是**建库属性**,由 MANIFEST 持久化;打开已有目录时无需再传(见 [16 §3](16-api-reference.md))。
- `filter!` 的表达式写死在代码里,非法字面量会 panic;运行时输入请用 `Expr::from_str`([06 §1.1](06-l4-query.md))。
- `close` 关闭的是共享库;其余克隆在关闭后不可再用(见 [16 §1.7](16-api-reference.md))。

---

## 1. 命名空间布局

推荐三层,按"生命周期"而非"数据类型"划分:

```text
agent-42/                      # 一个 Agent
├── profile/                   # 长期偏好与事实(高 importance,无 TTL)
├── session-88/                # 会话记忆(短 TTL,随会话结束清理)
└── knowledge/                 # 沉淀后的语义记忆(由 consolidate 产生)
```

```rust
let db = Mneme::builder().path("./agent_memory").dimension(1536)
    .fsync(FsyncPolicy::Batched(Duration::from_millis(20)))
    .build()?;                                   // 默认不自动遗忘,安全
let profile = db.namespace("agent-42/profile");
let session = db.namespace("agent-42/session-88");
```

> **为什么按生命周期分**:TTL、retention、compaction 都是按过滤/命名空间作用域设计的;
> 把"会过期的"和"永久的"混在一个空间里,任何遗忘策略都会误伤。

---

## 2. 会话记忆(episodic)

```rust
let mem = Record::new(embed("用户说他更喜欢深色模式")?)
    .key(format!("s88-{}", uuid))
    .text("用户说他更喜欢深色模式")
    .metadata(json!({"kind":"utterance","role":"user"}))
    .importance(0.4)
    .ttl(Duration::from_secs(7 * 86400));          // 一周后自动过期
session.insert(mem)?;
```

- **短 TTL** 交给引擎逻辑过期([07 §1](07-l5-life.md)),无需宿主定时任务;
- 会话结束时可选 `session.forget(filter!("kind == \"utterance\""))` 立即清理;
- 检索时用 `Scoring` 提高新鲜度权重,避免翻出旧会话:

```rust
let hits = session.search().vector(&q).text(&q_text).top_k(10)
    .score(Scoring { w_recency: 0.3, half_life: Duration::from_secs(3 * 86400), ..Scoring::default() })
    .execute()?;
```

---

## 3. 长期偏好/事实(semantic)与信念修订

```rust
profile.insert(
    Record::new(embed("用户喜欢深色模式")?)
        .key("pref.theme").text("用户喜欢深色模式")
        .metadata(json!({"kind":"preference"}))
        .importance(0.8).confidence(0.9)
)?;

// 用户改口了:用 supersede 保留"曾经喜欢浅色"的历史
profile.supersede("pref.theme",
    Record::new(embed("用户喜欢浅色模式")?)
        .key("pref.theme").text("用户喜欢浅色模式")
        .valid_from(now_ms()).importance(0.8).confidence(0.95))?;
```

- `supersede` = 更新 + 旧版本 `valid_to` 闭合([09 §3.3](09-memory-model.md));历史可经
  `db.as_of(ts)?` 回看;
- 高 `importance` 让它在排序里更靠前([10 §2](10-scoring.md)),也让它不易被
  保守遗忘策略清掉。

---

## 4. 去重与沉淀

```rust
// 写入期近似去重:同一偏好反复出现时合并,而不是堆一堆
let db = Mneme::builder().path("./m").dimension(1536)
    .dedup(Dedup::Merge(|old, new| {
        let mut r = new.to_record();
        r = r.importance(old.importance().max(new.importance()));
        Some(r)                                   // 保留更新时间与更高重要度
    }))
    .dedup_threshold(0.95)
    .build()?;
```

定期沉淀(episodic → semantic):

```rust
let report = session.consolidate(ConsolidationPolicy {
    filter: Some(filter!(r#"kind == "utterance""#)),
    threshold: 0.93,
    max_cluster: 32,
    target: Some("agent-42/knowledge".into()),                 // 摘要写入长期知识空间
    summarizer: Some(Arc::new(MySummarizer::new(embedder))),  // 宿主接 LLM/摘要模型
    keep_sources: true,                                       // 默认不删原始记忆
})?;
println!("沉淀 {} 簇,生成 {:?}", report.clusters, report.created);
```

- 沉淀不删除来源(`keep_sources=true`),只建立 `DERIVED_FROM` 边([09 §5](09-memory-model.md));
- `target` 指定摘要写入的命名空间(缺省 = 调用方所在命名空间);摘要记录写入
  `knowledge/`,由宿主决定何时清理会话空间。

---

## 5. 安全的遗忘策略

**默认关闭**自动遗忘([07 §3.4](07-l5-life.md))。若确需自动清理,推荐**作用域 + 白名单 + 保守阈值**:

```rust
let db = Mneme::builder().path("./m").dimension(1536)
    .retention(Some(
        Retention::new()
            .half_life(Duration::from_secs(30 * 86400))     // 30 天,比默认 14 天保守
            .min_importance(0.1)                            // 阈值更低
            .protect(filter!(r#"kind == "preference" || kind == "decision""#))
    ))
    .build()?;

// 或只对临时草稿空间手动清理
session.retain(Retention::new()
    .half_life(Duration::from_secs(14*86400))
    .min_importance(0.2)
    .protect(filter!(r#"pinned == true"#)))?;
```

- 删除可审计:`RetainReport.sampled_ids` 与 `iter_with(None, true)`([16 §1.3](16-api-reference.md));
- 用 `pinned: true` 元数据给"绝不能忘"的记忆上保险。

---

## 6. 混合检索 + 时序排序 + 联想

```rust
let hits = profile.search()
    .vector(&embed("他喜欢什么界面风格")?)
    .text("界面风格")
    .filter(filter!(r#"confidence > 0.6"#))
    .score(Scoring {
        w_recency: 0.2,
        w_importance: 0.3,
        w_access: 0.1,
        w_confidence: 0.2,
        ..Scoring::default()
    })
    .diversify(Diversity::Mmr { lambda: 0.7 })
    .expand(RelationExpand { hops: 1, kinds: vec![RelationKind::SUPPORTS, RelationKind::RELATED],
                             decay: 0.5, max_nodes: 64 })
    .top_k(10)
    .execute()?;
for h in &hits {
    println!("{} {:?} via={:?}", h.score, h.key, h.via);
}
```

- 过滤先行([06 §2](06-l4-query.md)),融合用 RRF([06 §4.1](06-l4-query.md)),再综合排序与 MMR;
- `Hit.via` 告诉你这条是被哪条边联想出来的。

---

## 7. 反馈闭环

```rust
let hits = session.search().vector(&q).top_k(10).execute()?;
let used = decide_which_were_useful(&hits);        // Agent 逻辑
for h in &hits {
    session.feedback(h.rowid,
        if used.contains(&h.rowid) { Feedback::Used } else { Feedback::Ignored },
        h.query_id)?;
}
```

- 反馈自动强化被用到的记忆([10 §4](10-scoring.md)),幂等键防重复计分;
- 与 `touch` 的区别:`feedback` 由检索结果驱动、可批量;`touch` 是宿主显式强化。

---

## 8. 运维

```rust
// 一致性备份(可独立打开)
let r = db.backup_to("./backup-2026-09-09")?;
assert!(r.files > 0);

// 健康检查与容量规划
let s = db.stats()?;
println!("segments={} wal={}B trash={}B retain={:?}",
         s.segments.len(), s.wal_bytes, s.trash_bytes, s.retain);
let report = db.check()?;                 // fsck
assert!(report.ok, "{:?}", report.suggestions);

// 优雅关闭:返回 Ok 即所有已确认写入持久
db.close()?;
```

- 备份目标必须不存在或为空([16 §7](16-api-reference.md));
- PITR:按目标时间点定期 `backup_to`,每份备份独立可开;单份备份**不能**把 `current`
  指回旧 MANIFEST(实时库的 MANIFEST 保留 2 版可供回滚,见 [16 §7.2](16-api-reference.md));
- 观测:`.observer(Arc::new(MyMetrics))` 接事件流(`Observer`;回调 panic 被
  `catch_unwind` 隔离,见 [12 §4](12-deployment.md))。

---

## 9. 部署形态

```rust
// 服务多 worker:一个写进程,多个只读进程
let ro = Mneme::builder().path("./agent_memory").read_only(true).build()?;
```

- 只读实例默认每 1s 探测一次新 MANIFEST 并原子切换视图(`Builder::read_only_probe_interval`
  可调,`Duration::ZERO` = 关闭),也可显式 `Mneme::reload()`;打开不创建/不争抢写锁,
  不随写者推进也能拿到新视图(见 [12 §2.1](12-deployment.md));
- 加密(feature `encrypt`):`Builder::encryption(Some(Encryption { provider: Arc::new(KmsKeyProvider), cipher: Cipher::Aes256Gcm }))`,
  密钥轮换用 `Mneme::rotate_encryption_key()`(需 provider 实现 `rotate`;见 [11 §2](11-security-storage.md));
- 压缩:`Builder::compression(Compression::Lz4)` 作用于记录体 `text`/`meta`/`provenance`,
  压缩无收益自动回退原文(feature `compress`/`compress-zstd`,见 [11 §3](11-security-storage.md))。

## 本章小结

- 命名空间按**生命周期**划分(profile/session/knowledge),避免遗忘策略误伤。
- 会话记忆:短 TTL + 新鲜度权重;长期偏好:`supersede` + 高 importance。
- 去重/沉淀、安全遗忘、混合检索 + 联想、反馈闭环、运维与部署各有配方。
- 所有配方都能从 §0 的端到端最小示例扩展而来。

## 下一章

[14-testing.md](14-testing.md):以上所有承诺的验收方法。
