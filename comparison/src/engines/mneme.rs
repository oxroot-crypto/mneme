//! Mneme 适配：持久库（临时目录）上批量写入 + `flush()` 建图。
//!
//! 构建时间口径 = `insert_batch` + `flush` 全程（含 WAL 写入、段落盘与 HNSW
//! 建图，即引擎的真实建库路径）；其他竞品为纯内存建图，报告备注此差异。
//! 两段耗时分别计时并经 [`Engine::build_phases`] 报告。

use std::collections::HashMap;
use std::time::Instant;

use ::mneme::{
    BuildPrecision, Builder, HnswParams, InsertOutcome, Metric, Mneme, Namespace, Record, Tuning,
    VectorFormat,
};

use super::{BuildOptions, Engine, Precision, Qualification};

/// Mneme 引擎句柄（持有库、命名空间与临时目录）。
pub struct MnemeEngine {
    db: Mneme,
    ns: Namespace,
    row_of_rowid: HashMap<u64, u32>,
    phases: crate::report::BuildPhases,
    /// 报告用引擎名（`mneme` / `mneme_i8` / `mneme_mem`）。
    label: &'static str,
    /// 持久库的临时目录；纯内存库为 `None`。
    _dir: Option<tempfile::TempDir>,
}

impl Engine for MnemeEngine {
    fn build(
        rows: usize,
        dim: usize,
        vectors: &[f32],
        threads: usize,
        _efs: &[usize],
        options: BuildOptions,
    ) -> Result<Self, String> {
        let dir = if options.in_memory {
            None
        } else {
            Some(
                tempfile::Builder::new()
                    .prefix("mneme-comparison-")
                    .tempdir()
                    .map_err(|error| format!("创建临时目录失败: {error}"))?,
            )
        };
        let mut builder = Builder::default()
            .dimension(dim as u32)
            .metric(Metric::Euclidean)
            .hnsw(HnswParams {
                m: 16,
                m0: 32,
                ef_construction: 200,
                ef_search: 128,
            })
            .parallelism(threads);
        if let Some(dir) = &dir {
            builder = builder.path(dir.path());
        }
        if options.quantization == Qualification::I8 {
            builder = builder.quantization(VectorFormat::I8Rescored);
        }
        if options.build_precision == Precision::F32 {
            builder = builder.build_precision(BuildPrecision::F32);
        }
        if !options.tuning.is_default() {
            let mut tuning = Tuning::default();
            let overrides = options.tuning;
            if let Some(value) = overrides.hnsw_batch_rows {
                tuning.hnsw_batch_rows = value;
            }
            if let Some(value) = overrides.hnsw_serial_rows {
                tuning.hnsw_serial_rows = value;
            }
            if let Some(value) = overrides.hnsw_threads_max {
                tuning.hnsw_threads_max = value;
            }
            if let Some(value) = overrides.flush_threads {
                tuning.flush_threads = value;
            }
            builder = builder.tuning(tuning);
        }
        let db = builder
            .build()
            .map_err(|error| format!("打开 Mneme 失败: {error}"))?;
        let ns = db.namespace("bench");
        let records: Vec<Record> = (0..rows)
            .map(|row| Record::new(vectors[row * dim..(row + 1) * dim].to_vec()))
            .collect();
        let insert_started = Instant::now();
        let outcomes = ns
            .insert_batch(records)
            .map_err(|error| format!("insert_batch 失败: {error}"))?;
        let insert_ms = insert_started.elapsed().as_secs_f64() * 1_000.0;
        let mut row_of_rowid = HashMap::with_capacity(rows);
        for (row, outcome) in outcomes.iter().enumerate() {
            match outcome {
                InsertOutcome::Inserted(rowid) | InsertOutcome::Merged(rowid) => {
                    row_of_rowid.insert(rowid.get(), row as u32);
                }
                InsertOutcome::Duplicate { .. } => {
                    return Err(format!("第 {row} 行被判定为重复,基准数据不应触发去重"));
                }
            }
        }
        let flush_started = Instant::now();
        db.flush().map_err(|error| format!("flush 失败: {error}"))?;
        let flush_ms = flush_started.elapsed().as_secs_f64() * 1_000.0;
        let label = if options.in_memory {
            "mneme_mem"
        } else {
            match options.quantization {
                Qualification::I8 => "mneme_i8",
                Qualification::F32 => "mneme",
            }
        };
        Ok(Self {
            db,
            ns,
            row_of_rowid,
            phases: crate::report::BuildPhases {
                insert_ms,
                flush_ms,
            },
            label,
            _dir: dir,
        })
    }

    fn name(&self) -> &'static str {
        self.label
    }

    fn version(&self) -> &'static str {
        // 与 ../Cargo.toml 的 version 同步(仓库内 path 依赖)。
        "0.1.0"
    }

    fn search(&self, query: &[f32], k: usize, ef: usize) -> Result<Vec<u32>, String> {
        let hits = self
            .ns
            .search()
            .vector(query)
            .top_k(k)
            .ef(ef)
            .execute()
            .map_err(|error| format!("查询失败: {error}"))?;
        hits.iter()
            .map(|hit| {
                self.row_of_rowid
                    .get(&hit.rowid.get())
                    .copied()
                    .ok_or_else(|| format!("命中未知 RowId {}", hit.rowid))
            })
            .collect()
    }

    fn index_bytes(&self) -> Option<u64> {
        self.db.stats().ok().map(|stats| stats.memory_est)
    }

    fn build_phases(&self) -> Option<crate::report::BuildPhases> {
        Some(self.phases)
    }
}
