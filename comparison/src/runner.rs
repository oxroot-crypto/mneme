//! 单引擎测量流程。
//!
//! 顺序：生成数据与真值 → 构建计时 → 各 ef 档预热 + 单线程计时/召回 →
//! 可选多线程吞吐 → 组装结果。构建与查询的进程内存（RSS）在各阶段边界采样。

use std::time::Instant;

use crate::cli::{Cli, Sizes};
use crate::dataset::{self, Dataset};
use crate::engines::{self, Engine};
use crate::metrics;
use crate::report::{EfResult, MultithreadResult, RunResult};

/// 预热查询条数（取数据集前 N 条）。
const WARMUP_QUERIES: usize = 64;

/// 运行单个引擎的完整测量。
///
/// # Errors
/// 数据集规模非法、引擎构建失败、查询失败或统计样本为空时返回错误说明。
pub fn run_one(cli: &Cli, engine_name: &str) -> Result<RunResult, String> {
    let sizes = &cli.sizes;
    let dataset = dataset::generate(
        sizes.data_mode,
        sizes.rows,
        sizes.dim,
        sizes.queries,
        sizes.k,
        sizes.threads,
    );
    let rss_before = metrics::rss_bytes();
    let build_started = Instant::now();
    let engine = engines::build_engine(
        engine_name,
        sizes.rows,
        sizes.dim,
        &dataset.vectors,
        sizes.threads,
        &sizes.efs,
        cli.build_options,
    )?;
    let build_ms = build_started.elapsed().as_secs_f64() * 1_000.0;
    let rss_after_build = metrics::rss_bytes();

    let mut per_ef = Vec::with_capacity(sizes.efs.len());
    for &ef in &sizes.efs {
        if !engine.supports_ef(ef) {
            per_ef.push(EfResult::skipped(ef));
            continue;
        }
        per_ef.push(measure_ef(&*engine, &dataset, ef, sizes)?);
    }

    let multithread = if sizes.qthreads > 1 {
        // 取中间档做并发吞吐样本(通常是召回门槛档 128)。
        let ef = sizes.efs[sizes.efs.len() / 2];
        let ef = if engine.supports_ef(ef) {
            ef
        } else {
            sizes.efs[0]
        };
        Some(measure_multithread(&*engine, &dataset, ef, sizes)?)
    } else {
        None
    };

    let rss_after_query = metrics::rss_bytes();
    let notes = notes_for(engine_name, sizes, &per_ef, cli.build_options);
    let mneme_options = engine_name.starts_with("mneme");
    Ok(RunResult {
        engine: engine.name().to_string(),
        version: engine.version().to_string(),
        data_mode: sizes.data_mode.label().to_string(),
        rows: dataset.rows,
        dim: dataset.dim,
        queries: sizes.queries,
        k: sizes.k,
        build_threads: sizes.threads,
        build_ms,
        build_rows_per_sec: sizes.rows as f64 / (build_ms / 1_000.0).max(f64::EPSILON),
        rss_before_bytes: rss_before,
        rss_after_build_bytes: rss_after_build,
        rss_after_query_bytes: rss_after_query,
        index_bytes_reported: engine.index_bytes(),
        per_ef,
        multithread,
        build_phases: engine.build_phases(),
        repeats: 1,
        build_ms_spread: None,
        multithread_qps_spread: None,
        quantization: effective_quantization(engine_name, cli.build_options).to_string(),
        build_precision: if mneme_options {
            cli.build_options.precision_name().to_string()
        } else {
            "—".to_string()
        },
        notes,
    })
}

/// 引擎实际生效的量化口径名（`mneme_i8` 别名经工厂强制 i8）。
fn effective_quantization(engine_name: &str, options: engines::BuildOptions) -> &'static str {
    if engine_name == "mneme_i8" {
        "i8"
    } else {
        options.quantization_name()
    }
}

/// 单 ef 档：预热后逐查询计时并累计召回。
fn measure_ef(
    engine: &dyn Engine,
    dataset: &Dataset,
    ef: usize,
    sizes: &Sizes,
) -> Result<EfResult, String> {
    let query_count = dataset.query_count();
    for index in 0..WARMUP_QUERIES.min(query_count) {
        engine.search(dataset.query(index), sizes.k, ef)?;
    }
    let mut samples_ns = Vec::with_capacity(query_count);
    let mut recall_sum = 0.0_f64;
    let mut hits_shortfall = 0_usize;
    for index in 0..query_count {
        let started = Instant::now();
        let hits = engine.search(dataset.query(index), sizes.k, ef)?;
        samples_ns.push(started.elapsed().as_nanos() as u64);
        if hits.len() < sizes.k {
            hits_shortfall += 1;
        }
        recall_sum += dataset::recall_at_k(&dataset.truth[index], &hits);
    }
    let latency = metrics::summarize(samples_ns);
    if latency.is_none() {
        return Err(format!("ef={ef} 无有效延迟样本"));
    }
    Ok(EfResult {
        ef,
        supported: true,
        recall: Some(recall_sum / query_count as f64),
        hits_shortfall,
        latency,
        recall_spread: None,
        p50_spread: None,
    })
}

/// 多线程并发吞吐：把查询均分给 `--qthreads` 个线程，测总墙钟。
fn measure_multithread(
    engine: &dyn Engine,
    dataset: &Dataset,
    ef: usize,
    sizes: &Sizes,
) -> Result<MultithreadResult, String> {
    let query_count = dataset.query_count();
    for index in 0..WARMUP_QUERIES.min(query_count) {
        engine.search(dataset.query(index), sizes.k, ef)?;
    }
    let threads = sizes.qthreads.clamp(1, query_count.max(1));
    let started = Instant::now();
    std::thread::scope(|scope| -> Result<(), String> {
        let chunk = query_count.div_ceil(threads);
        let mut handles = Vec::new();
        for worker in 0..threads {
            let start = worker * chunk;
            let end = query_count.min(start + chunk);
            if start >= end {
                break;
            }
            handles.push(scope.spawn(move || -> Result<(), String> {
                for index in start..end {
                    engine.search(dataset.query(index), sizes.k, ef)?;
                }
                Ok(())
            }));
        }
        let mut failure: Option<String> = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    failure.get_or_insert(error);
                }
                Err(_) => {
                    failure.get_or_insert_with(|| "查询线程 panic".to_string());
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    })?;
    let elapsed = started.elapsed().as_secs_f64().max(f64::EPSILON);
    Ok(MultithreadResult {
        ef,
        threads,
        qps: query_count as f64 / elapsed,
        qps_spread: None,
    })
}

/// 按引擎补充口径备注（写进报告）。
fn notes_for(
    engine: &str,
    sizes: &Sizes,
    per_ef: &[EfResult],
    options: engines::BuildOptions,
) -> Vec<String> {
    let mut notes = Vec::new();
    match engine {
        "mneme" | "mneme_i8" => {
            notes.push("构建为 insert_batch + flush，含 WAL 与段落盘".to_string());
            if !options.is_default() {
                notes.push(format!(
                    "显式配置：quant={}、build_precision={}",
                    effective_quantization(engine, options),
                    options.precision_name()
                ));
            }
            if !options.tuning.is_default() {
                notes.push(format!("建图调参：{}", options.tuning.summary()));
            }
        }
        "hnsw_rs" => {
            notes.push("parallel_insert 使用 rayon 全局线程池（按核数）".to_string());
        }
        "instant_distance" => {
            let supported: Vec<String> = per_ef
                .iter()
                .filter(|ef| ef.supported)
                .map(|ef| ef.ef.to_string())
                .collect();
            notes.push(format!(
                "M=32（库内常量，不可配）；ef 构建期锁定，仅支持 {{{}}} 档",
                supported.join(", ")
            ));
        }
        _ => {}
    }
    if sizes.threads != metrics::parallelism() {
        notes.push(format!("构建并行度显式设为 {}", sizes.threads));
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::build_engine;

    #[test]
    fn measure_ef_reports_full_recall_for_exact_search() {
        // 用库内向量自身当查询:精确检索必然自命中,mneme 小库走暴力档应满召回。
        let mut dataset = dataset::generate(crate::dataset::DataMode::Uniform, 256, 8, 16, 4, 2);
        dataset.queries = dataset.vectors[..16 * 8].to_vec();
        dataset.truth = dataset::brute_force_truth(&dataset.vectors, 8, &dataset.queries, 4, 2);
        let engine = build_engine(
            "mneme",
            256,
            8,
            &dataset.vectors,
            1,
            &[64],
            Default::default(),
        )
        .expect("构建");
        let sizes = Sizes {
            rows: 256,
            dim: 8,
            queries: 16,
            k: 4,
            efs: vec![64],
            data_mode: crate::dataset::DataMode::Uniform,
            threads: 1,
            qthreads: 1,
            repeats: 1,
        };
        let result = measure_ef(&*engine, &dataset, 64, &sizes).expect("测量成功");
        assert!(result.supported);
        assert_eq!(result.recall.expect("有召回值"), 1.0, "自查询应满召回");
        assert_eq!(result.hits_shortfall, 0);
        let latency = result.latency.expect("有延迟样本");
        assert_eq!(latency.samples, 16);
    }
}
