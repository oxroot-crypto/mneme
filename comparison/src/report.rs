//! 结果结构与报告渲染。
//!
//! 单引擎子进程产出 [`RunResult`]（JSON）；编排器汇总为 Markdown 报告并落盘：
//! `results/latest.json`、`results/run-<时间戳>.json`、`results/RESULTS.md`。

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::cli::Cli;
use crate::metrics::LatencyStats;

/// 单个 ef 档位的测量结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EfResult {
    /// 探查宽度。
    pub ef: usize,
    /// 该引擎是否支持此档（`instant_distance` 只支持构建期锁定的那一档）。
    pub supported: bool,
    /// Recall@k（逐查询平均）；不支持该档时为 `None`。
    pub recall: Option<f64>,
    /// 返回条数不足 k 的查询数。
    pub hits_shortfall: usize,
    /// 延迟汇总；不支持该档时为 `None`。
    pub latency: Option<LatencyStats>,
    /// 多轮重复时的 Recall 极差 `(min, max)`；单次运行为 `None`。
    #[serde(default)]
    pub recall_spread: Option<(f64, f64)>,
    /// 多轮重复时的 P50 极差 `(min, max)`（微秒）；单次运行为 `None`。
    #[serde(default)]
    pub p50_spread: Option<(f64, f64)>,
}

impl EfResult {
    /// 构造「引擎不支持该 ef 档」的结果。
    pub fn skipped(ef: usize) -> Self {
        Self {
            ef,
            supported: false,
            recall: None,
            hits_shortfall: 0,
            latency: None,
            recall_spread: None,
            p50_spread: None,
        }
    }
}

/// 多线程并发查询吞吐。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultithreadResult {
    /// 测试所用 ef 档。
    pub ef: usize,
    /// 并发线程数。
    pub threads: usize,
    /// 总吞吐（查询数 / 墙钟秒）。
    pub qps: f64,
    /// 多轮重复时的 QPS 极差 `(min, max)`；单次运行为 `None`。
    #[serde(default)]
    pub qps_spread: Option<(f64, f64)>,
}

/// 构建分段耗时（仅 Mneme 报告）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BuildPhases {
    /// `insert_batch` 耗时（毫秒）：WAL 追加 + 内存写状态。
    pub insert_ms: f64,
    /// `flush` 耗时（毫秒）：段编码 + HNSW 建图 + hidx 序列化 + 落盘。
    pub flush_ms: f64,
}

/// 单引擎完整测量结果（子进程 stdout 的 JSON 载荷）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    /// 引擎名。
    pub engine: String,
    /// 引擎版本（人工同步）。
    pub version: String,
    /// 数据形态（人类可读口径）。
    pub data_mode: String,
    /// 库内向量行数。
    pub rows: usize,
    /// 向量维度。
    pub dim: usize,
    /// 查询条数。
    pub queries: usize,
    /// 返回邻居数。
    pub k: usize,
    /// 构建并行度。
    pub build_threads: usize,
    /// 构建总耗时（毫秒）。
    pub build_ms: f64,
    /// 构建吞吐（行/秒）。
    pub build_rows_per_sec: f64,
    /// 构建前 RSS（字节）。
    pub rss_before_bytes: Option<u64>,
    /// 构建后 RSS（字节）。
    pub rss_after_build_bytes: Option<u64>,
    /// 全部测量结束后的 RSS（字节）。
    pub rss_after_query_bytes: Option<u64>,
    /// 引擎自报索引内存（字节）。
    pub index_bytes_reported: Option<u64>,
    /// 各 ef 档结果。
    pub per_ef: Vec<EfResult>,
    /// 多线程吞吐（`--qthreads > 1` 时）。
    pub multithread: Option<MultithreadResult>,
    /// 构建分段耗时（仅 Mneme；其余引擎或缺省时 `None`）。
    #[serde(default)]
    pub build_phases: Option<BuildPhases>,
    /// 聚合的重复轮数（1 = 单次运行；>1 = 各轮中位数）。
    #[serde(default = "one")]
    pub repeats: usize,
    /// 多轮重复时的构建耗时极差 `(min, max)`（毫秒）；单次运行为 `None`。
    #[serde(default)]
    pub build_ms_spread: Option<(f64, f64)>,
    /// 多轮重复时的 4 线程总吞吐极差 `(min, max)`（QPS）；单次运行为 `None`。
    #[serde(default)]
    pub multithread_qps_spread: Option<(f64, f64)>,
    /// 量化副本口径（`f32` / `i8`；非 Mneme 引擎为 `—`）。
    #[serde(default)]
    pub quantization: String,
    /// 建图精度口径（`f32` / `hybrid`；非 Mneme 引擎为 `—`）。
    #[serde(default)]
    pub build_precision: String,
    /// 口径备注。
    pub notes: Vec<String>,
}

/// `serde` 缺省值（旧 JSON 无 `repeats` 视为单次运行）。
fn one() -> usize {
    1
}

/// 把同一引擎的多轮运行聚合为中位数结果（编排器在 `--repeats > 1` 时调用）。
///
/// 约定：各轮必须同为同一引擎、同规模与同 ef 列表。数值字段取中位数；
/// `min_us` / `max_us` 取各轮极值的极值；并保留构建耗时、每档 P50 / Recall 与
/// 4 线程吞吐的 `(min, max)` 极差，供报告展示重复性。
///
/// # Errors
/// 输入为空或各轮规模/ef 列表不一致时返回错误说明。
pub fn aggregate(runs: &[RunResult]) -> Result<RunResult, String> {
    let Some(first) = runs.first() else {
        return Err("聚合输入为空".to_string());
    };
    if runs.len() == 1 {
        return Ok(first.clone());
    }
    for run in runs {
        if run.engine != first.engine
            || run.rows != first.rows
            || run.dim != first.dim
            || run.queries != first.queries
            || run.k != first.k
            || run.per_ef.len() != first.per_ef.len()
            || run
                .per_ef
                .iter()
                .zip(&first.per_ef)
                .any(|(a, b)| a.ef != b.ef)
        {
            return Err(format!("引擎 {} 的多轮结果规模不一致", first.engine));
        }
    }
    let build_values: Vec<f64> = runs.iter().map(|run| run.build_ms).collect();
    let build_ms = median(&build_values);
    let (build_min, build_max) = min_max(build_values.iter().copied());
    let per_ef = first
        .per_ef
        .iter()
        .enumerate()
        .map(|(index, template)| aggregate_ef(runs, index, template))
        .collect();
    let multithread = first.multithread.as_ref().map(|template| {
        let values: Vec<f64> = runs
            .iter()
            .filter_map(|run| run.multithread.as_ref().map(|mt| mt.qps))
            .collect();
        let (qps_min, qps_max) = min_max(values.iter().copied());
        MultithreadResult {
            ef: template.ef,
            threads: template.threads,
            qps: median(&values),
            qps_spread: (values.len() >= 2).then_some((qps_min, qps_max)),
        }
    });
    let build_phases = first.build_phases.map(|_| BuildPhases {
        insert_ms: median(&collect(runs, |run| {
            run.build_phases.map(|phases| phases.insert_ms)
        })),
        flush_ms: median(&collect(runs, |run| {
            run.build_phases.map(|phases| phases.flush_ms)
        })),
    });
    let mut notes = first.notes.clone();
    notes.push(format!("{} 轮重复取中位数（引擎轮转交错）", runs.len()));
    Ok(RunResult {
        engine: first.engine.clone(),
        version: first.version.clone(),
        data_mode: first.data_mode.clone(),
        rows: first.rows,
        dim: first.dim,
        queries: first.queries,
        k: first.k,
        build_threads: first.build_threads,
        build_ms,
        build_rows_per_sec: first.rows as f64 / (build_ms / 1_000.0).max(f64::EPSILON),
        rss_before_bytes: median_u64(runs.iter().filter_map(|run| run.rss_before_bytes)),
        rss_after_build_bytes: median_u64(runs.iter().filter_map(|run| run.rss_after_build_bytes)),
        rss_after_query_bytes: median_u64(runs.iter().filter_map(|run| run.rss_after_query_bytes)),
        index_bytes_reported: median_u64(runs.iter().filter_map(|run| run.index_bytes_reported)),
        per_ef,
        multithread,
        build_phases,
        repeats: runs.len(),
        build_ms_spread: Some((build_min, build_max)),
        multithread_qps_spread: spread_from(runs, |run| run.multithread.as_ref().map(|mt| mt.qps)),
        quantization: first.quantization.clone(),
        build_precision: first.build_precision.clone(),
        notes,
    })
}

/// 聚合单个 ef 档（`template` 来自首轮）。
fn aggregate_ef(runs: &[RunResult], index: usize, template: &EfResult) -> EfResult {
    if !template.supported {
        return EfResult::skipped(template.ef);
    }
    let latencies: Vec<&LatencyStats> = runs
        .iter()
        .filter_map(|run| run.per_ef[index].latency.as_ref())
        .collect();
    let Some(first_latency) = latencies.first() else {
        return EfResult::skipped(template.ef);
    };
    let latency = LatencyStats {
        samples: first_latency.samples,
        mean_us: median(
            &latencies
                .iter()
                .map(|stats| stats.mean_us)
                .collect::<Vec<_>>(),
        ),
        min_us: min_max(latencies.iter().map(|stats| stats.min_us)).0,
        p50_us: median(
            &latencies
                .iter()
                .map(|stats| stats.p50_us)
                .collect::<Vec<_>>(),
        ),
        p90_us: median(
            &latencies
                .iter()
                .map(|stats| stats.p90_us)
                .collect::<Vec<_>>(),
        ),
        p95_us: median(
            &latencies
                .iter()
                .map(|stats| stats.p95_us)
                .collect::<Vec<_>>(),
        ),
        p99_us: median(
            &latencies
                .iter()
                .map(|stats| stats.p99_us)
                .collect::<Vec<_>>(),
        ),
        max_us: min_max(latencies.iter().map(|stats| stats.max_us)).1,
        qps: median(&latencies.iter().map(|stats| stats.qps).collect::<Vec<_>>()),
    };
    let recalls: Vec<f64> = runs
        .iter()
        .filter_map(|run| run.per_ef[index].recall)
        .collect();
    EfResult {
        ef: template.ef,
        supported: true,
        recall: (!recalls.is_empty()).then(|| median(&recalls)),
        hits_shortfall: median_u64(
            runs.iter()
                .map(|run| run.per_ef[index].hits_shortfall as u64),
        )
        .unwrap_or(0) as usize,
        recall_spread: spread_from(runs, |run| run.per_ef[index].recall),
        p50_spread: spread_from(runs, |run| {
            run.per_ef[index].latency.as_ref().map(|stats| stats.p50_us)
        }),
        latency: Some(latency),
    }
}

/// 按中位数排序；偶数个取中间两个的平均（与 `LatencyStats` 同口径的常见约定）。
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    }
}

/// 极值 `(min, max)`；空集合返回 `(NaN, NaN)`。
fn min_max(values: impl Iterator<Item = f64>) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut seen = false;
    for value in values {
        min = min.min(value);
        max = max.max(value);
        seen = true;
    }
    if seen {
        (min, max)
    } else {
        (f64::NAN, f64::NAN)
    }
}

/// 多轮可选数值的极差；有效样本 < 2 或全缺失时为 `None`。
fn spread_from(runs: &[RunResult], pick: impl Fn(&RunResult) -> Option<f64>) -> Option<(f64, f64)> {
    let values: Vec<f64> = runs.iter().filter_map(&pick).collect();
    (values.len() >= 2).then(|| min_max(values.iter().copied()))
}

/// 从多轮结果收集某个可选数值（缺省跳过；全部缺省返回空 `Vec`）。
fn collect(runs: &[RunResult], pick: impl Fn(&RunResult) -> Option<f64>) -> Vec<f64> {
    runs.iter().filter_map(&pick).collect()
}

/// `u64` 中位数（空集合返回 `None`）。
fn median_u64(values: impl Iterator<Item = u64>) -> Option<u64> {
    let mut sorted: Vec<u64> = values.collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_unstable();
    Some(sorted[sorted.len() / 2])
}

/// 写入全部报告文件（聚合 JSON + 原始各轮 JSON + Markdown），并返回 Markdown 文本。
///
/// # Errors
/// 创建目录、写文件失败时返回错误说明。
pub fn write_reports(
    cli: &Cli,
    results: &[RunResult],
    raw: &[RunResult],
) -> Result<String, String> {
    std::fs::create_dir_all(&cli.out_dir)
        .map_err(|error| format!("创建输出目录 {:?} 失败: {error}", cli.out_dir))?;
    let json = serde_json::to_string_pretty(results)
        .map_err(|error| format!("序列化结果失败: {error}"))?;
    let stamp = unix_seconds();
    std::fs::write(cli.out_dir.join("latest.json"), &json)
        .map_err(|error| format!("写 latest.json 失败: {error}"))?;
    std::fs::write(cli.out_dir.join(format!("run-{stamp}.json")), &json)
        .map_err(|error| format!("写 run-{stamp}.json 失败: {error}"))?;
    let raw_json = serde_json::to_string_pretty(raw)
        .map_err(|error| format!("序列化原始轮次失败: {error}"))?;
    std::fs::write(cli.out_dir.join(format!("raw-{stamp}.json")), &raw_json)
        .map_err(|error| format!("写 raw-{stamp}.json 失败: {error}"))?;
    std::fs::write(cli.out_dir.join("raw.json"), &raw_json)
        .map_err(|error| format!("写 raw.json 失败: {error}"))?;
    let markdown = render_markdown(results, stamp);
    std::fs::write(cli.out_dir.join("RESULTS.md"), &markdown)
        .map_err(|error| format!("写 RESULTS.md 失败: {error}"))?;
    Ok(markdown)
}

/// 渲染 Markdown 报告。
pub fn render_markdown(results: &[RunResult], stamp: u64) -> String {
    let mut out = String::with_capacity(8 * 1024);
    let _ = writeln!(out, "# Mneme 与主流向量库性能对比（自动生成）\n");
    let _ = writeln!(
        out,
        "> 由 `comparison/` 工具在 {} 生成；聚合数据见同目录 `run-{}.json` / `latest.json`，全部原始轮次见 `raw.json`。\n",
        format_unix_utc(stamp),
        stamp
    );
    let _ = writeln!(out, "## 环境\n");
    let _ = writeln!(out, "| 项 | 值 |");
    let _ = writeln!(out, "| --- | --- |");
    let _ = writeln!(out, "| 主机 | {} |", crate::metrics::hostname());
    let _ = writeln!(out, "| CPU | {} |", crate::metrics::cpu_model());
    let _ = writeln!(out, "| 可用核数 | {} |", crate::metrics::parallelism());
    let _ = writeln!(out, "| rustc | {} |", rustc_version());
    if let Some(first) = results.first() {
        let _ = writeln!(
            out,
            "| 数据 | {} 行 × {} 维 f32（固定 seed，{}） |",
            first.rows, first.dim, first.data_mode
        );
        let _ = writeln!(
            out,
            "| 查询 | {} 条，top-{}，构建并行度 {} |",
            first.queries, first.k, first.build_threads
        );
        let efs: Vec<String> = first.per_ef.iter().map(|r| r.ef.to_string()).collect();
        let _ = writeln!(
            out,
            "| 参数 | M=16、ef_construction=200、ef∈{{{}}}、度量 L2² |",
            efs.join(", ")
        );
        if first.repeats > 1 {
            let _ = writeln!(
                out,
                "| 重复 | {} 轮（引擎轮转交错；正文表格取中位数，重复性见下） |",
                first.repeats
            );
        }
    }
    out.push('\n');

    let _ = writeln!(out, "## 构建\n");
    let _ = writeln!(
        out,
        "| 引擎 | 版本 | 构建耗时 | 构建吞吐 | 索引内存(自报) | RSS 增量 |"
    );
    let _ = writeln!(out, "| --- | --- | ---: | ---: | ---: | ---: |");
    for result in results {
        let rss_delta = match (result.rss_before_bytes, result.rss_after_build_bytes) {
            (Some(before), Some(after)) => format_bytes(after.saturating_sub(before)),
            _ => "—".to_string(),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            result.engine,
            result.version,
            format_duration_ms(result.build_ms),
            format_rate(result.build_rows_per_sec),
            result
                .index_bytes_reported
                .map(format_bytes)
                .unwrap_or_else(|| "—".to_string()),
            rss_delta
        );
    }
    out.push('\n');

    if results.iter().any(|result| result.build_phases.is_some()) {
        let _ = writeln!(out, "## 构建分段（mneme：insert_batch vs flush）\n");
        let _ = writeln!(
            out,
            "| 量化 | 建图精度 | insert_batch | flush | flush 占构建 |"
        );
        let _ = writeln!(out, "| --- | --- | ---: | ---: | ---: |");
        for result in results {
            let Some(phases) = result.build_phases else {
                continue;
            };
            let share = if result.build_ms > 0.0 {
                phases.flush_ms / result.build_ms * 100.0
            } else {
                0.0
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {:.1}% |",
                result.quantization,
                result.build_precision,
                format_duration_ms(phases.insert_ms),
                format_duration_ms(phases.flush_ms),
                share
            );
        }
        out.push('\n');
    }

    render_repeatability(&mut out, results);

    let _ = writeln!(out, "## 查询（单线程，含 Recall）\n");
    let _ = writeln!(
        out,
        "| 引擎 | ef | Recall@k | P50 | P90 | P95 | P99 | QPS | 返回不足 k |"
    );
    let _ = writeln!(
        out,
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    );
    for result in results {
        for ef in &result.per_ef {
            match (&ef.latency, ef.recall) {
                (Some(latency), Some(recall)) => {
                    let _ = writeln!(
                        out,
                        "| {} | {} | {:.4} | {} | {} | {} | {} | {} | {} |",
                        result.engine,
                        ef.ef,
                        recall,
                        format_micros(latency.p50_us),
                        format_micros(latency.p90_us),
                        format_micros(latency.p95_us),
                        format_micros(latency.p99_us),
                        format_qps(latency.qps),
                        ef.hits_shortfall
                    );
                }
                _ => {
                    let _ = writeln!(
                        out,
                        "| {} | {} | — | — | — | — | — | — | — |",
                        result.engine, ef.ef
                    );
                }
            }
        }
    }
    out.push('\n');

    if results.iter().any(|r| r.multithread.is_some()) {
        let _ = writeln!(out, "## 查询（多线程总吞吐）\n");
        let _ = writeln!(out, "| 引擎 | ef | 线程数 | 总 QPS |");
        let _ = writeln!(out, "| --- | ---: | ---: | ---: |");
        for result in results {
            if let Some(mt) = &result.multithread {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} |",
                    result.engine,
                    mt.ef,
                    mt.threads,
                    format_qps(mt.qps)
                );
            }
        }
        out.push('\n');
    }

    let _ = writeln!(out, "## 口径与备注\n");
    let _ = writeln!(
        out,
        "- 构建耗时：Mneme 含 WAL 与段落盘（真实建库路径，`insert_batch` + `flush`）；\
         其余竞品为纯内存建图，不落盘。\n\
         - 索引内存：各引擎自报口径不同（Mneme `memory_est`、usearch 分配器统计），\
         仅作量级参考；RSS 增量为子进程构建前后差值。\n\
         - 查询：单线程逐条墙钟计时；QPS 按均值折算；多线程为总吞吐（`--qthreads`）。\n\
         - `instant_distance`：`M` 为库内常量 32（不可配）；`ef` 构建期锁定，\
         只参与第一档；`hnsw_rs`：`parallel_insert` 使用 rayon 全局池（核数）。"
    );
    for result in results {
        if !result.notes.is_empty() {
            let _ = writeln!(out, "- {}：{}", result.engine, result.notes.join("；"));
        }
    }
    out.push('\n');
    out
}

/// 渲染「重复性」小节（仅 `--repeats > 1`）：构建耗时、4 线程吞吐与
/// 各 ef 档 P50 / Recall 的极差，用于判断中位数结论是否稳健。
fn render_repeatability(out: &mut String, results: &[RunResult]) {
    if results.first().is_none_or(|first| first.repeats <= 1) {
        return;
    }
    let _ = writeln!(out, "## 重复性（轮转交错，中位数与极差）\n");
    let _ = writeln!(
        out,
        "| 引擎 | 构建 min | 构建 中位 | 构建 max | 4 线程 QPS min | QPS 中位 | QPS max |"
    );
    let _ = writeln!(out, "| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
    for result in results {
        let (build_min, build_max) = result
            .build_ms_spread
            .unwrap_or((result.build_ms, result.build_ms));
        let (qps_min, qps_median, qps_max) = match &result.multithread {
            Some(mt) => {
                let (min, max) = mt.qps_spread.unwrap_or((mt.qps, mt.qps));
                (Some(min), Some(mt.qps), Some(max))
            }
            None => (None, None, None),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} |",
            result.engine,
            format_duration_ms(build_min),
            format_duration_ms(result.build_ms),
            format_duration_ms(build_max),
            qps_min.map_or("—".to_string(), format_qps),
            qps_median.map_or("—".to_string(), format_qps),
            qps_max.map_or("—".to_string(), format_qps),
        );
    }
    out.push('\n');
    let _ = writeln!(
        out,
        "| 引擎 | ef | P50 min | P50 中位 | P50 max | Recall min | Recall 中位 | Recall max |"
    );
    let _ = writeln!(
        out,
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    );
    for result in results {
        for ef in &result.per_ef {
            let Some(latency) = &ef.latency else {
                continue;
            };
            let (p50_min, p50_max) = ef.p50_spread.unwrap_or((latency.p50_us, latency.p50_us));
            let (recall_min, recall_median, recall_max) = match (ef.recall, ef.recall_spread) {
                (Some(recall), Some((min, max))) => (Some(min), Some(recall), Some(max)),
                (Some(recall), None) => (Some(recall), Some(recall), Some(recall)),
                (None, _) => (None, None, None),
            };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                result.engine,
                ef.ef,
                format_micros(p50_min),
                format_micros(latency.p50_us),
                format_micros(p50_max),
                recall_min.map_or("—".to_string(), |value| format!("{value:.4}")),
                recall_median.map_or("—".to_string(), |value| format!("{value:.4}")),
                recall_max.map_or("—".to_string(), |value| format!("{value:.4}")),
            );
        }
    }
    out.push('\n');
}

/// 打印字节数的人类可读形式。
fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value < KIB {
        format!("{bytes} B")
    } else if value < KIB * KIB {
        format!("{:.1} KiB", value / KIB)
    } else if value < KIB * KIB * KIB {
        format!("{:.1} MiB", value / (KIB * KIB))
    } else {
        format!("{:.2} GiB", value / (KIB * KIB * KIB))
    }
}

/// 毫秒耗时的人类可读形式（<1s 用 ms，否则用 s）。
fn format_duration_ms(ms: f64) -> String {
    if ms < 1_000.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{:.2} s", ms / 1_000.0)
    }
}

/// 每秒行数。
fn format_rate(rows_per_sec: f64) -> String {
    if rows_per_sec >= 10_000.0 {
        format!("{:.0} 行/s", rows_per_sec)
    } else {
        format!("{:.1} 行/s", rows_per_sec)
    }
}

/// 微秒延迟的人类可读形式。
fn format_micros(us: f64) -> String {
    if us < 1_000.0 {
        format!("{us:.1} µs")
    } else {
        format!("{:.2} ms", us / 1_000.0)
    }
}

/// 每秒查询数。
fn format_qps(qps: f64) -> String {
    if qps >= 10_000.0 {
        format!("{qps:.0}")
    } else {
        format!("{qps:.1}")
    }
}

/// 当前 Unix 秒。
fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// `rustc --version` 输出（取不到时返回 `unknown`）。
fn rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Unix 秒转 `YYYY-MM-DD HH:MM:SS UTC`（无第三方日期库的民用日历换算）。
fn format_unix_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Howard Hinnant `civil_from_days`：Unix 天数 → (年, 月, 日)。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::summarize;

    fn sample_result() -> RunResult {
        RunResult {
            engine: "mneme".to_string(),
            version: "0.1.0".to_string(),
            data_mode: "测试数据".to_string(),
            rows: 100,
            dim: 8,
            queries: 10,
            k: 3,
            build_threads: 2,
            build_ms: 1_500.0,
            build_rows_per_sec: 66.7,
            rss_before_bytes: Some(1_000),
            rss_after_build_bytes: Some(2_048),
            rss_after_query_bytes: Some(3_072),
            index_bytes_reported: Some(4_096),
            per_ef: vec![EfResult {
                ef: 64,
                supported: true,
                recall: Some(0.95),
                hits_shortfall: 0,
                latency: summarize(vec![1_000, 2_000, 3_000]),
                recall_spread: Some((0.94, 0.96)),
                p50_spread: Some((1.5, 2.5)),
            }],
            multithread: None,
            build_phases: Some(BuildPhases {
                insert_ms: 500.0,
                flush_ms: 1_000.0,
            }),
            repeats: 2,
            build_ms_spread: Some((1_400.0, 1_600.0)),
            multithread_qps_spread: None,
            quantization: "i8".to_string(),
            build_precision: "hybrid".to_string(),
            notes: vec!["测试备注".to_string()],
        }
    }

    #[test]
    fn markdown_contains_core_sections() {
        let markdown = render_markdown(&[sample_result()], 0);
        assert!(markdown.contains("## 环境"));
        assert!(markdown.contains("## 构建"));
        assert!(markdown.contains("## 查询"));
        assert!(markdown.contains("mneme"));
        assert!(markdown.contains("0.9500"));
        assert!(markdown.contains("测试备注"));
    }

    /// 多轮运行渲染「重复性」小节（构建与 P50/Recall 极差）。
    #[test]
    fn markdown_renders_repeatability() {
        let markdown = render_markdown(&[sample_result()], 0);
        assert!(markdown.contains("## 重复性"));
        assert!(markdown.contains("1.40 s"));
        assert!(markdown.contains("1.60 s"));
        assert!(markdown.contains("0.9400"));
        assert!(markdown.contains("0.9600"));
        assert!(markdown.contains("2 轮"));
    }

    /// 聚合取中位数并保留极差;单轮直接返回原值。
    #[test]
    fn aggregate_takes_median_with_spread() {
        let mut slow = sample_result();
        slow.build_ms = 2_000.0;
        slow.per_ef[0].latency = summarize(vec![3_000, 4_000, 5_000]);
        slow.per_ef[0].recall = Some(0.90);
        let mut fast = sample_result();
        fast.build_ms = 1_000.0;
        fast.per_ef[0].latency = summarize(vec![1_000, 2_000, 3_000]);
        fast.per_ef[0].recall = Some(1.0);
        let mut mid = sample_result();
        mid.build_ms = 1_500.0;
        mid.per_ef[0].latency = summarize(vec![2_000, 2_000, 2_000]);
        mid.per_ef[0].recall = Some(0.95);
        let aggregated = aggregate(&[slow, fast, mid]).expect("聚合成功");
        assert_eq!(aggregated.repeats, 3);
        assert!((aggregated.build_ms - 1_500.0).abs() < 1e-9, "构建取中位");
        assert_eq!(aggregated.build_ms_spread, Some((1_000.0, 2_000.0)));
        let ef = &aggregated.per_ef[0];
        assert!((ef.latency.as_ref().expect("延迟").p50_us - 2.0).abs() < 1e-9);
        assert!((ef.recall.expect("召回") - 0.95).abs() < 1e-9);
        assert_eq!(ef.recall_spread, Some((0.90, 1.0)));
        assert_eq!(ef.p50_spread, Some((2.0, 4.0)));
    }

    /// 单轮聚合为恒等（不引入 `spread` 字段）。
    #[test]
    fn aggregate_single_run_is_identity() {
        let aggregated = aggregate(&[sample_result()]).expect("聚合成功");
        assert_eq!(aggregated.repeats, 2, "保留输入自身的重复标记");
        assert_eq!(aggregated.build_ms, 1_500.0);
    }

    /// 构建分段只在有 `build_phases` 时渲染，并给出 flush 占比。
    #[test]
    fn markdown_renders_build_phases() {
        let markdown = render_markdown(&[sample_result()], 0);
        assert!(markdown.contains("## 构建分段"));
        assert!(markdown.contains("| i8 | hybrid | 500.0 ms | 1.00 s | 66.7% |"));
    }

    #[test]
    fn civil_calendar_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
    }

    #[test]
    fn formats_are_human_readable() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1_536), "1.5 KiB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.0 MiB");
        assert_eq!(format_duration_ms(999.0), "999.0 ms");
        assert_eq!(format_duration_ms(2_000.0), "2.00 s");
        assert_eq!(format_micros(123.4), "123.4 µs");
        assert_eq!(format_micros(2_000.0), "2.00 ms");
    }
}
