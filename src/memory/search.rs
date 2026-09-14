//! 暴力扫描 / ANN 检索(`search.rs`)。
//!
//! **过滤先行**:先用元数据求值得到候选位图(不读向量),再分块并行打分、
//! `TopK` 归并。这是设计 03 §4 的 L1 落地。L3 起:若读视图带有覆盖槽位前缀的
//! 向量索引且前缀规模超过 `brute_force_max_rows`,前缀走 HNSW(过滤三档),
//! 未建树的尾部仍暴力扫描,二者 `TopK` 归并——语义与纯暴力统计等价(设计 05 §9)。

use crate::core::bitset::BitSet;
use crate::core::error::{MnemeError, Result};
use crate::core::heap::TopK;
use crate::core::metric::{Metric, Score};
use crate::core::options::VectorFormat;
use crate::core::simd;
use crate::core::types::{NsId, RowId, SlotId};
use crate::memory::index::IndexSearch;
use crate::memory::pred::{self, EvalCtx, Expr};
use crate::memory::table::ReaderView;

// 单测操作计数:统计扫描阶段的打分次数(线程局部,避免测试间干扰)。
#[cfg(test)]
thread_local! {
    static SCORE_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static COARSE_CANDIDATES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn bump_score_calls() {
    SCORE_CALLS.with(|calls| calls.set(calls.get() + 1));
}

#[cfg(test)]
fn record_coarse_candidates(k: usize) {
    COARSE_CANDIDATES.with(|calls| calls.set(k));
}

/// 一条被打分的候选。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scored {
    /// 物理槽位。
    pub(crate) slot: SlotId,
    /// 稳定逻辑标识。
    pub(crate) rowid: RowId,
    /// 原始相似度分。
    pub(crate) score: Score,
}

/// 计算向量范数平方。
pub(crate) fn norm_sq(vector: &[f32]) -> f32 {
    simd::dot(vector, vector)
}

/// 一次检索的全部输入。
pub(crate) struct SearchParams<'a> {
    /// 不可变读视图。
    pub(crate) view: &'a ReaderView,
    /// 目标命名空间。
    pub(crate) ns_id: NsId,
    /// 查询向量。
    pub(crate) query: &'a [f32],
    /// 距离度量。
    pub(crate) metric: Metric,
    /// 返回条数。
    pub(crate) top_k: usize,
    /// ANN 探查宽度(仅前缀走索引时生效)。
    pub(crate) ef: usize,
    /// 预过滤表达式。
    pub(crate) filter: Option<&'a Expr>,
    /// 当前时刻(Unix 毫秒)。
    pub(crate) now_ms: i64,
    /// 并行分块行数。
    pub(crate) block: usize,
    /// 线程数;`0` = 自动。
    pub(crate) parallelism: usize,
    /// 段行数低于此值恒暴力扫描(设计 05 §11)。
    pub(crate) brute_force_max_rows: usize,
    /// 过滤三档:后过滤 / 放大后过滤分界。
    pub(crate) filter_post_threshold: f32,
    /// 过滤三档:放大后过滤 / 候选暴力分界。
    pub(crate) filter_brute_threshold: f32,
    /// 两阶段粗排过采样倍率(粗排候选 = `top_k × 本值`;仅存在量化段时生效)。
    pub(crate) rescore_oversample: usize,
    /// 预计算的候选槽位(过滤先行,与 BM25 通道共享);
    /// `None` = 本函数内自行收集。
    pub(crate) candidates: Option<&'a [u32]>,
    /// 重要性偏置(只改 HNSW 遍历顺序;`Scoring::bias_routing`,FC-SCORE-POST-007)。
    pub(crate) bias: Option<&'a dyn crate::memory::index::NodeBias>,
}

/// 过滤先行的候选收集输入(与 BM25 通道共享同一份候选)。
pub(crate) struct CandidateQuery<'a> {
    /// 不可变读视图。
    pub(crate) view: &'a ReaderView,
    /// 目标命名空间。
    pub(crate) ns_id: NsId,
    /// 预过滤表达式。
    pub(crate) filter: Option<&'a Expr>,
    /// 当前时刻(Unix 毫秒)。
    pub(crate) now_ms: i64,
}

/// 分块扫描的输入(顺序与并行共用)。
struct ScanParams<'a> {
    view: &'a ReaderView,
    candidates: &'a [u32],
    query: &'a [f32],
    query_norm: f32,
    metric: Metric,
    k: usize,
    chunk: usize,
    threads: usize,
}

/// 在给定快照上做过滤 + 检索,返回按 `Metric::better` 排序的 top-k。
pub(crate) fn search(params: &SearchParams<'_>) -> Result<Vec<Scored>> {
    let query_norm = if params.metric.needs_norm() {
        norm_sq(params.query)
    } else {
        0.0
    };

    let candidates_owned;
    let candidates: &[u32] = match params.candidates {
        Some(list) => list,
        None => {
            candidates_owned = collect_candidates(&CandidateQuery {
                view: params.view,
                ns_id: params.ns_id,
                filter: params.filter,
                now_ms: params.now_ms,
            });
            &candidates_owned
        }
    };
    let k = params.top_k.min(candidates.len());
    if k == 0 {
        return Ok(Vec::new());
    }

    // L3/L5:各已落盘段图分别 ANN,未覆盖槽位(未落盘尾部/无 hidx 段)暴力,
    // 归并后统一重取分(多段形态下语义与全量暴力统计等价,设计 05 §9)。
    let indexed: usize = params
        .view
        .indexes
        .iter()
        .map(|segment| segment.covered.count_ones())
        .sum();
    if indexed > params.brute_force_max_rows {
        return search_indexed(params, candidates, query_norm);
    }

    let top = run_scan(params, candidates, query_norm, k)?;
    Ok(rescore(params, top, query_norm))
}

/// ANN 路径:存在量化段时粗排放宽到 `top_k × rescore_oversample`(默认 4 倍,即
/// 设计中的「4k」),f32 精排按 f32 分重排再截回 `top_k`(I12 / FC-QUANT-INV-015);
/// 无量化段时口径与 L3 完全一致。
fn search_indexed(
    params: &SearchParams<'_>,
    candidates: &[u32],
    query_norm: f32,
) -> Result<Vec<Scored>> {
    let quantized = params
        .view
        .indexes
        .iter()
        .any(|segment| segment.quant != VectorFormat::F32);
    let coarse_k = if quantized {
        crate::quant::rescore::coarse_candidates(params.top_k, params.rescore_oversample)
            .min(candidates.len())
    } else {
        params.top_k.min(candidates.len())
    };
    let budget = AnnBudget {
        query_norm,
        k: coarse_k,
    };
    let mut scored = ann_search(params, candidates, budget)?;
    scored.truncate(params.top_k);
    Ok(scored)
}

/// 过滤先行:只求值元数据,得到候选槽位(不读向量)。
pub(crate) fn collect_candidates(query: &CandidateQuery<'_>) -> Vec<u32> {
    let mut candidates: Vec<u32> = Vec::new();
    let uses_access = query.filter.is_some_and(pred::Expr::uses_access);
    for (idx, slot) in query.view.slots.iter().enumerate() {
        if query.view.dead.get(idx) || slot.ns_id != query.ns_id || !slot.is_live(query.now_ms) {
            continue;
        }
        if let Some(expr) = query.filter {
            let ctx = EvalCtx {
                slot,
                access: uses_access
                    .then(|| query.view.access.get(&slot.rowid).copied())
                    .flatten(),
            };
            if !pred::matches(expr, &ctx) {
                continue;
            }
        }
        // 槽位下标 ≤ u32::MAX:commit_version 经 `slot_id_for` 拒绝继续增长,
        // 下标越界即违反 FC-MEM-INV-004,故此转换可证明不会失败。
        candidates.push(u32::try_from(idx).expect("槽位下标必可转入 u32(FC-MEM-INV-004)"));
    }
    candidates
}

/// ANN 路径的预算参数:`search` 已算好查询范数与有效 `k`,打包传入避免参数堆叠。
struct AnnBudget {
    query_norm: f32,
    k: usize,
}

/// ANN 路径:逐段图搜索 + 未覆盖候选暴力,归并后再取分。
fn ann_search(
    params: &SearchParams<'_>,
    candidates: &[u32],
    budget: AnnBudget,
) -> Result<Vec<Scored>> {
    let AnnBudget { query_norm, k } = budget;
    #[cfg(test)]
    record_coarse_candidates(k);
    let mut top = TopK::new(k, params.metric);
    let mut covered = BitSet::default();
    for segment in params.view.indexes.iter() {
        // 每段只处理与该段覆盖相交的候选;`alive` 亦限制在本段覆盖内,
        // 避免选择性口径被其他段的位图稀释(过滤三档按段独立分派)。
        let (alive, filter) = segment_bitmaps(params, segment, candidates);
        let partial = segment.index.search(&IndexSearch {
            query: params.query,
            query_norm,
            ef: params.ef,
            k,
            alive: &alive,
            filter: filter.as_ref(),
            post_threshold: params.filter_post_threshold,
            brute_threshold: params.filter_brute_threshold,
            use_quant: segment.quant != VectorFormat::F32,
            bias: params.bias,
        });
        top.merge(partial);
        covered.union_with(&segment.covered);
    }

    let tail: Vec<u32> = candidates
        .iter()
        .copied()
        .filter(|&idx| !covered.get(idx as usize))
        .collect();
    if !tail.is_empty() {
        top.merge(run_scan(params, &tail, query_norm, k)?);
    }
    Ok(rescore(params, top, query_norm))
}

/// 构造单个段索引的 `alive` 位图与候选过滤位图(均按全局槽位)。
fn segment_bitmaps(
    params: &SearchParams<'_>,
    segment: &crate::memory::index::SegmentIndex,
    candidates: &[u32],
) -> (BitSet, Option<BitSet>) {
    let mut alive = BitSet::default();
    for slot in &segment.slots {
        let idx = slot.get() as usize;
        let Some(slot_data) = params.view.slots.get(idx) else {
            continue;
        };
        if !params.view.dead.get(idx)
            && slot_data.ns_id == params.ns_id
            && slot_data.is_live(params.now_ms)
        {
            alive.set(idx);
        }
    }
    // 仅当存在用户过滤(`filter`)时构造索引段过滤位图:exec 无过滤时传全 1
    // 候选且 `filter = None`,不得把"全 1 候选"误当过滤(否则会触发契约
    // FC-INDEX-POST-001 的档③「候选暴力」路径)。
    let filter = params.filter.is_some().then(|| {
        let mut bits = BitSet::default();
        for &idx in candidates {
            let idx = idx as usize;
            if segment.covered.get(idx) {
                bits.set(idx);
            }
        }
        bits
    });
    (alive, filter)
}

/// 决定分块大小与线程数并执行一次暴力扫描。
fn run_scan(
    params: &SearchParams<'_>,
    candidates: &[u32],
    query_norm: f32,
    k: usize,
) -> Result<TopK<(RowId, SlotId)>> {
    let (chunk, threads) = plan_scan(params);
    let scan = ScanParams {
        view: params.view,
        candidates,
        query: params.query,
        query_norm,
        metric: params.metric,
        k,
        chunk,
        threads,
    };
    if candidates.len() <= chunk || threads <= 1 {
        Ok(scan_sequential(&scan))
    } else {
        // 工作线程异常终止 = 打分阶段内部不变量被破坏;按 FC-GLOBAL-ERR-001 不向外
        // 传播 panic,也不静默返回偏少的结果,而是转为结构化错误(FC-MEM-INV-004 口径)。
        scan_parallel(&scan)
    }
}

/// 根据候选规模决定分块大小与线程数。
fn plan_scan(params: &SearchParams<'_>) -> (usize, usize) {
    let chunk = params.block.max(1);
    let threads = if cfg!(feature = "wasm") {
        1
    } else if params.parallelism == 0 {
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    } else {
        params.parallelism
    };
    (chunk, threads)
}

/// 两阶段精排排序键:先按 [`Metric::score_order`] 的全序(`NaN` 恒排最后),
/// 再按 `RowId` 升序去平。与 [`TopK`] 选用同一全序,保证「入选集合」与
/// 「最终排序」一致。
fn compare_scored(metric: Metric, left: &Scored, right: &Scored) -> std::cmp::Ordering {
    metric
        .score_order(left.score, right.score)
        .then_with(|| left.rowid.cmp(&right.rowid))
}

/// 重新取分:粗排 `TopK` 只提供候选,分数一律用 f32 原向量在候选集内重算并
/// **按 f32 分重排**(两阶段第二阶段;I12 / FC-QUANT-INV-015)。
///
/// 排序口径与 [`TopK`] 一致:良者在前(`Metric::better` 方向),同分按 `RowId` 升序。
fn rescore(params: &SearchParams<'_>, top: TopK<(RowId, SlotId)>, query_norm: f32) -> Vec<Scored> {
    let mut scored: Vec<Scored> = top
        .into_sorted_vec()
        .into_iter()
        .map(|(rowid, slot)| {
            let slot_data = &params.view.slots[slot.get() as usize];
            let score = params.metric.score(
                params.query,
                &slot_data.vector,
                query_norm,
                slot_data.norm_sq,
            );
            Scored { slot, rowid, score }
        })
        .collect();
    scored.sort_by(|left, right| compare_scored(params.metric, left, right));
    scored
}

fn scan_sequential(params: &ScanParams<'_>) -> TopK<(RowId, SlotId)> {
    let mut top = TopK::new(params.k, params.metric);
    for &idx in params.candidates {
        let (rowid, slot, score) = score_slot(params, idx);
        top.push(score, (rowid, slot));
    }
    top
}

/// 对候选 `idx` 打分;单测时累计操作计数(FC-MEM-CPLX-001)。
fn score_slot(params: &ScanParams<'_>, idx: u32) -> (RowId, SlotId, Score) {
    let slot = SlotId::new(idx);
    let slot_data = &params.view.slots[idx as usize];
    let score = params.metric.score(
        params.query,
        &slot_data.vector,
        params.query_norm,
        slot_data.norm_sq,
    );
    #[cfg(test)]
    bump_score_calls();
    (slot_data.rowid, slot, score)
}

fn scan_parallel(params: &ScanParams<'_>) -> Result<TopK<(RowId, SlotId)>> {
    let chunk = params
        .chunk
        .max(params.candidates.len().div_ceil(params.threads));
    let partials = std::thread::scope(|scope| {
        let handles: Vec<_> = params
            .candidates
            .chunks(chunk)
            .map(|part| scope.spawn(move || scan_chunk(params, part)))
            .collect();
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(local) => Ok(local),
                // 工作线程 panic:打分阶段的内部不变量被破坏,绝不静默吞掉
                // (静默吞掉会返回偏少的结果,违反「拒绝静默失败」)。
                Err(_) => Err(MnemeError::Inconsistent {
                    reason: "并行扫描工作线程异常终止",
                }),
            })
            .collect::<Result<Vec<_>>>()
    })?;

    let mut top = TopK::new(params.k, params.metric);
    for partial in partials {
        top.merge(partial);
    }
    Ok(top)
}

/// 对单个候选分片打分为局部 `TopK`。
fn scan_chunk(params: &ScanParams<'_>, part: &[u32]) -> TopK<(RowId, SlotId)> {
    let mut local = TopK::new(params.k, params.metric);
    for &idx in part {
        let (rowid, slot, score) = score_slot(params, idx);
        local.push(score, (rowid, slot));
    }
    local
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_sq_of_unit_basis_is_one() {
        assert!((norm_sq(&[1.0, 0.0, 0.0]) - 1.0).abs() < 1e-6);
    }

    /// FC-MEM-CPLX-001(操作计数:扫描打分次数 = 候选数,无隐藏全扫)
    #[test]
    fn scan_touches_each_candidate_once() {
        let db = crate::memory::Mneme::in_memory(2).expect("in_memory");
        let ns = db.namespace("n");
        for index in 0..8 {
            ns.insert(crate::memory::Record::new(vec![1.0, 0.0]).key(format!("k{index}")))
                .expect("insert");
        }
        SCORE_CALLS.with(|calls| calls.set(0));
        let hits = ns
            .search()
            .vector(&[1.0, 0.0])
            .top_k(4)
            .execute()
            .expect("search");
        assert_eq!(hits.len(), 4);
        let calls = SCORE_CALLS.with(std::cell::Cell::get);
        assert_eq!(calls, 8, "扫描打分次数必须等于候选数(线性,无隐藏全扫)");
    }

    /// FC-INDEX-POST-009(分派):段行数超过 `brute_force_max_rows` 且视图带索引时,
    /// 查询必须走索引搜索,不得回退为全量暴力重扫——否则恒暴力退化解无人察觉。
    #[test]
    fn search_dispatches_to_index_when_prefix_exceeds_brute_threshold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::memory::Builder::default()
            .path(dir.path())
            .dimension(4)
            .tuning(crate::core::options::Tuning {
                brute_force_max_rows: 8,
                ..crate::core::options::Tuning::default()
            })
            .build()
            .expect("build");
        let ns = db.namespace("n");
        let batch: Vec<crate::memory::Record> = (0..64)
            .map(|row| crate::memory::Record::new(vec![row as f32, 1.0, 0.0, 0.0]))
            .collect();
        ns.insert_batch(batch).expect("insert_batch");
        db.flush().expect("flush");

        SCORE_CALLS.with(|calls| calls.set(0));
        let hits = ns
            .search()
            .vector(&[1.0, 0.0, 0.0, 0.0])
            .top_k(4)
            .ef(16)
            .execute()
            .expect("search");
        assert_eq!(hits.len(), 4);
        let calls = SCORE_CALLS.with(std::cell::Cell::get);
        assert_eq!(
            calls, 0,
            "有索引且段行数超过阈值时必须走索引,不得回退暴力重扫前缀"
        );
    }

    /// FC-INDEX-POST-009(分派边界):段行数等于 `brute_force_max_rows` 时仍走暴力
    /// (契约口径为严格「超过」),且索引存在与否不改变该边界判定。
    #[test]
    fn search_bruteforces_when_prefix_does_not_exceed_threshold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::memory::Builder::default()
            .path(dir.path())
            .dimension(4)
            .tuning(crate::core::options::Tuning {
                brute_force_max_rows: 64,
                ..crate::core::options::Tuning::default()
            })
            .build()
            .expect("build");
        let ns = db.namespace("n");
        let batch: Vec<crate::memory::Record> = (0..64)
            .map(|row| crate::memory::Record::new(vec![row as f32, 1.0, 0.0, 0.0]))
            .collect();
        ns.insert_batch(batch).expect("insert_batch");
        db.flush().expect("flush");

        SCORE_CALLS.with(|calls| calls.set(0));
        let hits = ns
            .search()
            .vector(&[1.0, 0.0, 0.0, 0.0])
            .top_k(4)
            .ef(16)
            .execute()
            .expect("search");
        assert_eq!(hits.len(), 4);
        let calls = SCORE_CALLS.with(std::cell::Cell::get);
        assert_eq!(
            calls, 64,
            "行数等于阈值(未严格超过)时必须走暴力;若索引存在即走图会在此变红"
        );
    }

    /// FC-QUANT-INV-015:两阶段粗排候选数 `= min(top_k × oversample, 候选总数)`,
    /// 且仍返回 top_k;倍率为 8、top_k=4 时候选恰为 32,不得放大成全量扫描。
    #[test]
    fn coarse_candidates_respect_rescore_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::memory::Builder::default()
            .path(dir.path())
            .dimension(4)
            .quantization(crate::core::options::VectorFormat::I8Rescored)
            .tuning(crate::core::options::Tuning {
                brute_force_max_rows: 8,
                rescore_oversample: 8,
                quant_recall_floor: 0.0,
                ..crate::core::options::Tuning::default()
            })
            .build()
            .expect("build");
        let ns = db.namespace("n");
        let batch: Vec<crate::memory::Record> = (0..128)
            .map(|row| crate::memory::Record::new(vec![row as f32, 1.0, (row % 7) as f32, 1.0]))
            .collect();
        ns.insert_batch(batch).expect("insert_batch");
        db.flush().expect("flush");

        COARSE_CANDIDATES.with(|calls| calls.set(0));
        let hits = ns
            .search()
            .vector(&[1.0, 0.0, 0.0, 0.0])
            .top_k(4)
            .ef(16)
            .execute()
            .expect("search");
        assert_eq!(hits.len(), 4);
        let coarse = COARSE_CANDIDATES.with(std::cell::Cell::get);
        assert_eq!(coarse, 32, "粗排候选应为 top_k × oversample = 32");
    }

    /// FC-QUANT-INV-015(重排口径):精排排序键按度量方向 + `RowId` 去平,
    /// 与粗排插入次序无关;分数含 `NaN` 时仍给出确定性全序(不破坏 `sort_by`
    /// 的严格弱序要求)。
    #[test]
    fn rescore_ordering_is_metric_aware_and_total() {
        let scored = |rowid: u64, score: f32| Scored {
            slot: SlotId::new(rowid as u32),
            rowid: RowId::new(rowid),
            score,
        };
        let mut dot = [
            scored(1, 0.5),
            scored(2, 2.0),
            scored(3, 2.0),
            scored(4, f32::NAN),
        ];
        dot.sort_by(|left, right| compare_scored(Metric::Dot, left, right));
        assert_eq!(
            dot.iter().map(|hit| hit.rowid.get()).collect::<Vec<_>>(),
            vec![2, 3, 1, 4],
            "Dot:高分在前,同分按 RowId 升序,NaN 排末"
        );
        let mut euclidean = [scored(1, 0.5), scored(2, 2.0)];
        euclidean.sort_by(|left, right| compare_scored(Metric::Euclidean, left, right));
        assert_eq!(euclidean[0].rowid.get(), 1, "Euclidean:低分(更近)在前");
    }
}
