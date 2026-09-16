//! Mneme 竞品对比工具入口（开发期基准，见 `comparison/README.md`）。
//!
//! - `run`：编排器。为每个引擎 spawn 一个独立子进程（RSS 与计时互不污染），
//!   收集 JSON 结果并写出 `RESULTS.md` + 原始 JSON；
//! - `run-one --engine <名>`：单引擎测量，向 stdout 打印一行 `@@RESULT@@<json>`。
//!
//! 公平性口径、参数选择与结果解读见 `comparison/README.md`。

mod cli;
mod dataset;
mod engines;
mod metrics;
mod report;
mod runner;

use std::process::Command;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = dispatch(args) {
        eprintln!("错误: {error}");
        std::process::exit(1);
    }
}

/// 按子命令分派。
fn dispatch(args: Vec<String>) -> Result<(), String> {
    let cli = cli::parse(args)?;
    match &cli.mode {
        cli::Mode::RunOne { engine } => {
            let result = runner::run_one(&cli, engine)?;
            let json = serde_json::to_string(&result)
                .map_err(|error| format!("序列化结果失败: {error}"))?;
            println!("@@RESULT@@{json}");
        }
        cli::Mode::Run => {
            let (results, raw) = orchestrate(&cli)?;
            let markdown = report::write_reports(&cli, &results, &raw)?;
            println!("{markdown}");
            eprintln!(
                "[comparison] 报告已写入 {}/RESULTS.md（原始各轮数据见 raw.json）",
                cli.out_dir.display()
            );
        }
    }
    Ok(())
}

/// 轮转运行每个引擎的子进程并聚合结果（串行以保计时与内存口径干净）。
///
/// `--repeats N` 时按「轮 → 引擎」两层循环交错执行（而非逐引擎跑满 N 轮），
/// 使各引擎共享同一时段的机器状态，再对每引擎取中位数与极差。
/// 返回 `(各引擎聚合中位数, 全部原始轮次)`。
fn orchestrate(cli: &cli::Cli) -> Result<(Vec<report::RunResult>, Vec<report::RunResult>), String> {
    let repeats = cli.sizes.repeats.max(1);
    let mut runs_by_engine: Vec<Vec<report::RunResult>> =
        (0..cli.engines.len()).map(|_| Vec::new()).collect();
    let mut raw = Vec::with_capacity(cli.engines.len() * repeats);
    for round in 1..=repeats {
        for (slot, engine) in cli.engines.iter().enumerate() {
            eprintln!("[comparison] 第 {round}/{repeats} 轮 · 引擎 {engine} ...");
            let started = Instant::now();
            let result = run_engine_once(cli, engine)?;
            eprintln!(
                "[comparison] 第 {round}/{repeats} 轮 · 引擎 {engine}: 完成（{:.1}s）",
                started.elapsed().as_secs_f64()
            );
            runs_by_engine[slot].push(result.clone());
            raw.push(result);
        }
    }
    let aggregated = runs_by_engine
        .iter()
        .map(|runs| report::aggregate(runs))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((aggregated, raw))
}

/// 以独立子进程运行单个引擎一轮，解析其 `@@RESULT@@` JSON 结果。
fn run_engine_once(cli: &cli::Cli, engine: &str) -> Result<report::RunResult, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("定位当前可执行文件失败: {error}"))?;
    let sizes = &cli.sizes;
    let efs = sizes
        .efs
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut command = Command::new(&executable);
    command
        .arg("run-one")
        .arg("--engine")
        .arg(engine)
        .arg("--rows")
        .arg(sizes.rows.to_string())
        .arg("--dim")
        .arg(sizes.dim.to_string())
        .arg("--queries")
        .arg(sizes.queries.to_string())
        .arg("--k")
        .arg(sizes.k.to_string())
        .arg("--ef")
        .arg(&efs)
        .arg("--data-mode")
        .arg(sizes.data_mode.as_name())
        .arg("--threads")
        .arg(sizes.threads.to_string())
        .arg("--qthreads")
        .arg(sizes.qthreads.to_string())
        .arg("--quant")
        .arg(cli.build_options.quantization_name())
        .arg("--build-precision")
        .arg(cli.build_options.precision_name());
    let tuning = &cli.build_options.tuning;
    for (flag, value) in [
        ("--hnsw-batch-rows", tuning.hnsw_batch_rows),
        ("--hnsw-serial-rows", tuning.hnsw_serial_rows),
        ("--hnsw-threads-max", tuning.hnsw_threads_max),
        ("--flush-threads", tuning.flush_threads),
    ] {
        if let Some(value) = value {
            command.arg(flag).arg(value.to_string());
        }
    }
    let output = command
        .output()
        .map_err(|error| format!("启动 {engine} 子进程失败: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "引擎 {engine} 失败(退出码 {:?}):\n{stderr}",
            output.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let payload = stdout
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("@@RESULT@@"))
        .ok_or_else(|| format!("引擎 {engine} 未输出 @@RESULT@@ 标记"))?;
    serde_json::from_str(payload).map_err(|error| format!("解析 {engine} 结果失败: {error}"))
}
