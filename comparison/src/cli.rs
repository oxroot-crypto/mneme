//! 命令行解析。
//!
//! 两个子命令：
//! - `run`：编排器，为每个引擎 spawn 一个独立子进程（内存与计时互不污染），
//!   收集结果并落盘报告；
//! - `run-one --engine <名>`：单个引擎的构建 + 查询测量，只向 stdout 打印一行
//!   `@@RESULT@@<json>`（其余输出被忽略），供编排器收集。
//!
//! 参数校验失败一律返回 `Err(String)`，由 `main` 打印到 stderr 并以非零码退出。

use std::path::PathBuf;

use crate::dataset::DataMode;
use crate::engines::{BuildOptions, Precision, Qualification};
use crate::metrics;

/// 参与对比的引擎名（与 `engines` 模块的工厂一致）。
///
/// `mneme_i8` 为 Mneme 的 i8 量化副本口径（`VectorFormat::I8Rescored`）别名，
/// 与默认 f32 口径在同一轮编排内轮转交错测量，保证两条曲线同机同时段可比。
pub const ALL_ENGINES: &[&str] = &[
    "mneme",
    "mneme_i8",
    "mneme_mem",
    "usearch",
    "hnsw_rs",
    "hnsw_stable",
    "instant_distance",
];

/// 默认探查宽度档位（64 = 主 crate 默认值，128 = 召回门槛档，256 = 高召回档）。
pub const DEFAULT_EFS: &[usize] = &[64, 128, 256];

/// 数据集与测量规模。
#[derive(Debug, Clone)]
pub struct Sizes {
    /// 库内向量行数。
    pub rows: usize,
    /// 向量维度。
    pub dim: usize,
    /// 查询条数。
    pub queries: usize,
    /// 每条查询返回的邻居数。
    pub k: usize,
    /// 查询探查宽度档位。
    pub efs: Vec<usize>,
    /// 数据形态。
    pub data_mode: DataMode,
    /// 建索引并行度。
    pub threads: usize,
    /// 并发查询线程数（1 = 只测单线程）。
    pub qthreads: usize,
    /// 每引擎重复轮数（≥1）；>1 时轮转交错各引擎、报告取中位数与极差。
    pub repeats: usize,
}

/// 运行模式。
#[derive(Debug)]
pub enum Mode {
    /// 编排全部引擎。
    Run,
    /// 只运行指定引擎（子进程模式）。
    RunOne {
        /// 引擎名。
        engine: String,
    },
}

/// 解析后的命令行。
#[derive(Debug)]
pub struct Cli {
    /// 运行模式。
    pub mode: Mode,
    /// 规模参数。
    pub sizes: Sizes,
    /// 参与对比的引擎（`run` 模式生效）。
    pub engines: Vec<String>,
    /// Mneme 专属构建选项（其余引擎忽略）。
    pub build_options: BuildOptions,
    /// 报告输出目录。
    pub out_dir: PathBuf,
}

/// 解析参数（不含 argv[0]）。
///
/// # Errors
/// 未知子命令、未知参数、缺参、非法数值、空引擎列表或未知引擎名时返回错误说明。
pub fn parse(args: Vec<String>) -> Result<Cli, String> {
    let Some(command) = args.first() else {
        return Err(usage());
    };
    if command == "--help" || command == "-h" || command == "help" {
        return Err(usage());
    }
    let mode = match command.as_str() {
        "run" => Mode::Run,
        "run-one" => Mode::RunOne {
            engine: String::new(),
        },
        other => return Err(format!("未知子命令 `{other}`\n{}", usage())),
    };
    let mut cli = Cli {
        mode,
        sizes: Sizes {
            rows: 50_000,
            dim: 128,
            queries: 1_000,
            k: 10,
            efs: DEFAULT_EFS.to_vec(),
            data_mode: DataMode::Uniform,
            threads: metrics::parallelism(),
            qthreads: 1,
            repeats: 1,
        },
        engines: ALL_ENGINES.iter().map(|s| s.to_string()).collect(),
        build_options: BuildOptions::default(),
        out_dir: PathBuf::from("results"),
    };
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("参数 `{flag}` 缺少取值"))?;
        match flag {
            "--engine" => {
                cli.mode = Mode::RunOne {
                    engine: value.clone(),
                };
            }
            "--rows" => cli.sizes.rows = parse_usize(flag, value)?,
            "--dim" => cli.sizes.dim = parse_usize(flag, value)?,
            "--queries" => cli.sizes.queries = parse_usize(flag, value)?,
            "--k" => cli.sizes.k = parse_usize(flag, value)?,
            "--threads" => cli.sizes.threads = parse_usize(flag, value)?,
            "--qthreads" => cli.sizes.qthreads = parse_usize(flag, value)?,
            "--repeats" => cli.sizes.repeats = parse_usize(flag, value)?,
            "--ef" => {
                cli.sizes.efs = value
                    .split(',')
                    .map(|part| parse_usize(flag, part.trim()))
                    .collect::<Result<Vec<_>, _>>()?;
            }
            "--data-mode" => {
                cli.sizes.data_mode = DataMode::from_name(value)
                    .ok_or_else(|| format!("未知数据形态 `{value}`；可选 uniform / clustered"))?;
            }
            "--engines" => {
                cli.engines = value.split(',').map(|s| s.trim().to_string()).collect();
            }
            "--quant" => {
                cli.build_options.quantization = match value.as_str() {
                    "f32" => Qualification::F32,
                    "i8" => Qualification::I8,
                    other => {
                        return Err(format!("未知量化口径 `{other}`；可选：f32 / i8"));
                    }
                };
            }
            "--build-precision" => {
                cli.build_options.build_precision = match value.as_str() {
                    "f32" => Precision::F32,
                    "hybrid" => Precision::Hybrid,
                    other => {
                        return Err(format!("未知建图精度 `{other}`；可选：f32 / hybrid"));
                    }
                };
            }
            "--hnsw-batch-rows" => {
                cli.build_options.tuning.hnsw_batch_rows = Some(parse_usize(flag, value)?)
            }
            "--hnsw-serial-rows" => {
                cli.build_options.tuning.hnsw_serial_rows = Some(parse_usize(flag, value)?);
            }
            "--hnsw-threads-max" => {
                cli.build_options.tuning.hnsw_threads_max = Some(parse_usize(flag, value)?);
            }
            "--flush-threads" => {
                cli.build_options.tuning.flush_threads = Some(parse_usize(flag, value)?)
            }
            "--out" => cli.out_dir = PathBuf::from(value),
            other => return Err(format!("未知参数 `{other}`\n{}", usage())),
        }
        index += 2;
    }
    validate(&cli)?;
    Ok(cli)
}

fn parse_usize(flag: &str, value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("参数 `{flag}` 取值 `{value}` 不是合法非负整数"))
}

fn validate(cli: &Cli) -> Result<(), String> {
    let sizes = &cli.sizes;
    if sizes.rows == 0 || sizes.dim == 0 || sizes.queries == 0 || sizes.k == 0 {
        return Err("rows / dim / queries / k 都必须 > 0".to_string());
    }
    if sizes.efs.is_empty() || sizes.efs.contains(&0) {
        return Err("--ef 至少一档且都必须 > 0".to_string());
    }
    if sizes.threads == 0 {
        return Err("--threads 必须 > 0".to_string());
    }
    if sizes.qthreads == 0 {
        return Err("--qthreads 必须 > 0".to_string());
    }
    if sizes.repeats == 0 {
        return Err("--repeats 必须 > 0".to_string());
    }
    if cli.engines.is_empty() {
        return Err("--engines 不能为空".to_string());
    }
    for engine in &cli.engines {
        if !ALL_ENGINES.contains(&engine.as_str()) {
            return Err(format!(
                "未知引擎 `{engine}`；可选：{}",
                ALL_ENGINES.join(", ")
            ));
        }
    }
    if let Mode::RunOne { engine } = &cli.mode
        && !ALL_ENGINES.contains(&engine.as_str())
    {
        return Err(format!(
            "未知引擎 `{engine}`；可选：{}",
            ALL_ENGINES.join(", ")
        ));
    }
    Ok(())
}

/// 用法说明。
pub fn usage() -> String {
    format!(
        "用法:\n  run [--rows N] [--dim N] [--queries N] [--k N] [--ef a,b,c]\n      [--data-mode uniform|clustered] [--engines a,b,c]\n      [--quant f32|i8] [--build-precision f32|hybrid]\n      [--hnsw-batch-rows N] [--hnsw-serial-rows N] [--hnsw-threads-max N] [--flush-threads N]\n      [--threads N] [--qthreads N] [--repeats N] [--out DIR]\n  run-one --engine NAME [同上规模参数]\n\n引擎: {}",
        ALL_ENGINES.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_defaults_for_run() {
        let cli = parse(args(&["run"])).expect("解析成功");
        assert!(matches!(cli.mode, Mode::Run));
        assert_eq!(cli.sizes.rows, 50_000);
        assert_eq!(cli.sizes.efs, vec![64, 128, 256]);
        assert_eq!(cli.engines.len(), ALL_ENGINES.len());
    }

    #[test]
    fn parses_overrides_and_run_one() {
        let cli = parse(args(&[
            "run-one",
            "--engine",
            "usearch",
            "--rows",
            "2000",
            "--dim",
            "64",
            "--queries",
            "50",
            "--k",
            "5",
            "--ef",
            "32, 64",
            "--data-mode",
            "clustered",
            "--quant",
            "i8",
            "--build-precision",
            "f32",
            "--hnsw-batch-rows",
            "512",
            "--hnsw-serial-rows",
            "128",
            "--hnsw-threads-max",
            "8",
            "--flush-threads",
            "2",
            "--threads",
            "2",
            "--qthreads",
            "4",
            "--repeats",
            "3",
            "--out",
            "/tmp/opencode/out",
        ]))
        .expect("解析成功");
        match cli.mode {
            Mode::RunOne { engine } => assert_eq!(engine, "usearch"),
            Mode::Run => panic!("应为 run-one"),
        }
        assert_eq!(cli.sizes.rows, 2_000);
        assert_eq!(cli.sizes.dim, 64);
        assert_eq!(cli.sizes.efs, vec![32, 64]);
        assert_eq!(cli.sizes.data_mode, DataMode::Clustered);
        assert_eq!(cli.sizes.qthreads, 4);
        assert_eq!(cli.sizes.repeats, 3);
        assert_eq!(cli.build_options.quantization, Qualification::I8);
        assert_eq!(cli.build_options.build_precision, Precision::F32);
        assert_eq!(
            (
                cli.build_options.tuning.hnsw_batch_rows,
                cli.build_options.tuning.hnsw_serial_rows,
                cli.build_options.tuning.hnsw_threads_max,
                cli.build_options.tuning.flush_threads,
            ),
            (Some(512), Some(128), Some(8), Some(2))
        );
        assert_eq!(cli.out_dir, PathBuf::from("/tmp/opencode/out"));
    }

    #[test]
    fn rejects_unknown_engine_and_bad_numbers() {
        assert!(parse(args(&["run", "--engines", "faiss"])).is_err());
        assert!(parse(args(&["run", "--rows", "abc"])).is_err());
        assert!(parse(args(&["run", "--rows"])).is_err());
        assert!(parse(args(&["run", "--data-mode", "grid"])).is_err());
        assert!(parse(args(&["run", "--quant", "i4"])).is_err());
        assert!(parse(args(&["run", "--build-precision", "fast"])).is_err());
        assert!(parse(args(&["run", "--repeats", "0"])).is_err());
        assert!(parse(args(&["nope"])).is_err());
    }
}
