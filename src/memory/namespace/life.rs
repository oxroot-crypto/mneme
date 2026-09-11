//! `Namespace` 生命周期:主动遗忘与记忆沉淀(`namespace/life.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::Meta;
use crate::core::options::RelationKind;
use crate::core::types::{NsId, RowId};
use crate::memory::lifecycle::{RetainReport, Retention, retention_score};
use crate::memory::mutate_helpers::{build_summary, is_consolidated};
use crate::memory::pred::{self, EvalCtx, Expr};
use crate::memory::relation::Edge;
use crate::memory::score::{self, ConsolidateReport, ConsolidationPolicy};
use crate::memory::table::{SlotData, WriterState};
use crate::memory::write_helpers::{SlotSpec, build_slot};

use super::Namespace;

impl Namespace {
    /// 主动遗忘:对过滤器命中的每行打墓碑,返回删除数。
    ///
    /// # Arguments
    /// * `filter` - 命中即遗忘的三值过滤表达式。
    ///
    /// # Returns
    /// 打墓碑的记录数(命中的活记录数)。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Expr, Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// assert_eq!(ns.forget(Expr::field("key").eq("a")).unwrap(), 1);
    /// ```
    pub fn forget(&self, filter: Expr) -> Result<usize> {
        let config = Arc::clone(&self.config);
        let ns_path = Arc::clone(&self.ns_path);
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            let Some(ns_id) = ws.ns_id_of(&ns_path) else {
                return Ok(0);
            };
            let now = config.clock.now_unix_ms();
            let victims: Vec<RowId> = ws
                .slots
                .iter()
                .enumerate()
                .filter(|(idx, slot)| {
                    !ws.dead.get(*idx)
                        && slot.ns_id == ns_id
                        && slot.is_live(now)
                        && pred::matches(
                            &filter,
                            &EvalCtx {
                                slot,
                                access: ws.access.get(&slot.rowid).copied(),
                            },
                        )
                })
                .map(|(_, slot)| slot.rowid)
                .collect();
            let count = victims.len();
            for rowid in victims {
                let seqno = ws.alloc_seqno();
                ws.tombstone(rowid, now, seqno)?;
            }
            Ok(count)
        })
    }

    /// 按遗忘策略回收低保留分记录。
    ///
    /// # Arguments
    /// * `policy` - 遗忘策略(保留分阈值与下限)。
    ///
    /// # Returns
    /// 执行报告:扫描数、遗忘数与抽样 `RowId`(可审计,I23)。
    ///
    /// # Errors
    /// 库已关闭 → [`MnemeError::Closed`];`min_importance` 或 `access_weight` 含非有限值(NaN)→
    /// [`MnemeError::Config`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record, Retention};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0])).unwrap();
    /// let report = ns.retain(Retention::new()).unwrap();
    /// assert_eq!(report.scanned, 1);
    /// ```
    pub fn retain(&self, policy: Retention) -> Result<RetainReport> {
        let config = Arc::clone(&self.config);
        let ns_path = Arc::clone(&self.ns_path);
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            // 非有限值阈值会让「score < 阈值」恒为假、遗忘静默失效;`access_weight` 非有限值
            // 同样会让保留分恒为 NaN,一并显式拒绝(FC-LIFE-POST-002 / FC-GLOBAL-PRE-004)。
            if !policy.min_importance.is_finite() || !policy.access_weight.is_finite() {
                return Err(MnemeError::Config {
                    reason: "min_importance 与 access_weight 必须是有限值",
                });
            }
            let Some(ns_id) = ws.ns_id_of(&ns_path) else {
                return Ok(RetainReport::default());
            };
            let now = config.clock.now_unix_ms();
            let mut report = RetainReport::default();
            let (scanned, victims) = collect_retain_victims(ws, ns_id, now, &policy);
            report.scanned = scanned;
            for rowid in victims {
                let seqno = ws.alloc_seqno();
                if ws.tombstone(rowid, now, seqno)? {
                    report.forgotten += 1;
                    report.sampled_ids.push(rowid);
                }
            }
            Ok(report)
        })
    }

    /// 记忆沉淀:把近似重复的记忆聚簇、合并/摘要,并链接来源(设计 09 §5)。
    ///
    /// # Arguments
    /// * `policy` - 沉淀策略(聚类阈值、簇上限与可选摘要器)。
    ///
    /// # Returns
    /// 沉淀报告:簇数、被合并来源数与新摘要的 `RowId` 列表。
    ///
    /// # Errors
    /// 库已关闭 → [`MnemeError::Closed`];策略参数非法(`threshold` 非 `[0,1]`
    /// 内的有限值或 `max_cluster = 0`)→ [`MnemeError::Config`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{ConsolidationPolicy, Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0])).unwrap();
    /// ns.insert(Record::new(vec![1.0, 0.001])).unwrap();
    /// let report = ns.consolidate(ConsolidationPolicy::default()).unwrap();
    /// assert_eq!(report.clusters, 1);
    /// ```
    pub fn consolidate(&self, policy: ConsolidationPolicy) -> Result<ConsolidateReport> {
        let config = Arc::clone(&self.config);
        let ns_path = Arc::clone(&self.ns_path);
        self.table.write_tx(move |ws| {
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            // 策略参数非法时显式拒绝,绝不静默空转或索引越界 panic(FC-MODEL-POST-006):
            // threshold=NaN 使 `相似度 ≥ 阈值` 恒为假,越界值超出相似度口径;
            // max_cluster=0 会把候选簇截断为空簇。
            // NaN 的 `contains` 恒为 false,一个区间判断即可同时覆盖非有限值与越界。
            if !(0.0..=1.0).contains(&policy.threshold) || policy.max_cluster == 0 {
                return Err(MnemeError::Config {
                    reason: "沉淀策略非法:threshold 必须为 [0,1] 内的有限值且 max_cluster ≥ 1",
                });
            }
            let Some(ns_id) = ws.ns_id_of(&ns_path) else {
                return Ok(ConsolidateReport::default());
            };
            let now = config.clock.now_unix_ms();
            let candidates = collect_consolidation_candidates(ws, ns_id, now, &policy);
            let vectors: Vec<&[f32]> = candidates
                .iter()
                .map(|slot_data| slot_data.vector.as_ref())
                .collect();
            let clusters = score::cluster_by_similarity(&vectors, policy.threshold);
            let target_path = consolidation_target(&ns_path, &policy);
            let target_id = ws.register_ns(&target_path)?;
            let mut report = ConsolidateReport::default();
            {
                let mut ctx = ConsolidationCtx {
                    ws,
                    policy: &policy,
                    candidates: &candidates,
                    target_id,
                    target_path: &target_path,
                    now,
                };
                for cluster in clusters {
                    if cluster.len() >= 2 {
                        ctx.merge_cluster(&cluster, &mut report)?;
                    }
                }
            }
            Ok(report)
        })
    }
}

/// 收集低于保留分、且未被 `protect` 豁免的记录;返回 `(扫描数, 待遗忘 RowId)`。
fn collect_retain_victims(
    ws: &WriterState,
    ns_id: NsId,
    now: i64,
    policy: &Retention,
) -> (usize, Vec<RowId>) {
    let mut scanned = 0;
    let mut victims = Vec::new();
    for (idx, slot) in ws.slots.iter().enumerate() {
        if ws.dead.get(idx) || slot.ns_id != ns_id || !slot.is_live(now) {
            continue;
        }
        scanned += 1;
        if let Some(protect) = &policy.protect
            && pred::matches(
                protect,
                &EvalCtx {
                    slot,
                    access: ws.access.get(&slot.rowid).copied(),
                },
            )
        {
            continue;
        }
        let access = ws.access.get(&slot.rowid).copied().unwrap_or_default();
        let age = now - slot.valid_from.max(access.last_access_ms);
        let score = retention_score(slot.importance, age, access.access_count, policy);
        if score < policy.min_importance {
            victims.push(slot.rowid);
        }
    }
    (scanned, victims)
}

/// 收集满足过滤的活记录作为沉淀候选(逻辑过期/墓碑记录一律排除,I9)。
fn collect_consolidation_candidates(
    ws: &WriterState,
    ns_id: NsId,
    now: i64,
    policy: &ConsolidationPolicy,
) -> Vec<Arc<SlotData>> {
    ws.slots
        .iter()
        .enumerate()
        .filter(|(idx, slot)| {
            !ws.dead.get(*idx)
                && slot.ns_id == ns_id
                && slot.is_live(now)
                && policy.filter.as_ref().is_none_or(|expr| {
                    pred::matches(
                        expr,
                        &EvalCtx {
                            slot,
                            access: ws.access.get(&slot.rowid).copied(),
                        },
                    )
                })
        })
        .map(|(_, slot)| Arc::clone(slot))
        .collect()
}

/// 摘要写入的目标命名空间路径。
fn consolidation_target(ns_path: &Arc<str>, policy: &ConsolidationPolicy) -> Arc<str> {
    policy
        .target
        .as_ref()
        .map_or_else(|| Arc::clone(ns_path), |path| Arc::from(path.as_str()))
}

/// 单次 `consolidate` 的可变上下文(写状态 + 策略 + 候选)。
struct ConsolidationCtx<'a> {
    ws: &'a mut WriterState,
    policy: &'a ConsolidationPolicy,
    candidates: &'a [Arc<SlotData>],
    target_id: NsId,
    target_path: &'a Arc<str>,
    now: i64,
}

impl ConsolidationCtx<'_> {
    /// 合并一个连通分量:生成摘要、链接来源、按需墓碑来源。
    fn merge_cluster(&mut self, cluster: &[usize], report: &mut ConsolidateReport) -> Result<()> {
        let candidates = self.candidates;
        let policy = self.policy;
        let mut members: Vec<&Arc<SlotData>> =
            cluster.iter().map(|idx| &candidates[*idx]).collect();
        if members.len() > policy.max_cluster {
            members.sort_by(|a, b| {
                b.importance
                    .partial_cmp(&a.importance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            members.truncate(policy.max_cluster);
        }
        if members
            .iter()
            .any(|slot_data| is_consolidated(self.ws, slot_data.rowid))
        {
            return Ok(());
        }
        let source_ids: Vec<RowId> = members.iter().map(|slot_data| slot_data.rowid).collect();
        let summary = build_summary(policy, &members);
        let summary_rowid = self.ws.alloc_rowid();
        let seqno = self.ws.alloc_seqno();
        let slot_data = build_slot(SlotSpec {
            ns_id: self.target_id,
            ns_path: Arc::clone(self.target_path),
            rowid: summary_rowid,
            seqno,
            tx_ms: self.now,
            rec: summary,
        });
        self.ws.commit_version(summary_rowid, slot_data)?;
        self.link_sources(summary_rowid, &source_ids);
        if !policy.keep_sources {
            for source in &source_ids {
                let seqno = self.ws.alloc_seqno();
                self.ws.tombstone(*source, self.now, seqno)?;
            }
        }
        report.clusters += 1;
        report.merged += source_ids.len();
        report.created.push(summary_rowid);
        Ok(())
    }

    /// 以 `DERIVED_FROM` 边把摘要链接到各来源。
    fn link_sources(&mut self, summary_rowid: RowId, source_ids: &[RowId]) {
        for source in source_ids {
            let edge = Edge {
                from: summary_rowid,
                to: *source,
                kind: RelationKind::DERIVED_FROM,
                weight: 1.0,
                metadata: Meta::Null,
            };
            self.ws.relate_edge(edge);
        }
    }
}
