//! 各引擎适配层：统一接口、统一参数口径。
//!
//! 公平性口径（详见 `comparison/README.md`）：
//! - 度量一律 L2²（欧氏距离平方，排序与 L2 等价）；
//! - 图参数一律 `M=16`、`ef_construction=200`（`instant-distance` 的 `M` 为库内
//!   常量 32，无法配置，报告备注）；
//! - 构建并行度用 `--threads`；查询单线程计时，QPS 为单线程吞吐；
//! - 返回值一律为数据集行号（0-based），便于与 ground truth 对比召回。

pub mod hnswlib;
pub mod hnswrs;
pub mod instant;
pub mod mneme;
pub mod usearch;

/// Mneme 专属构建选项（其余引擎忽略）。
#[derive(Debug, Clone, Copy, Default)]
pub struct BuildOptions {
    /// 段量化副本格式：`f32`（默认）或 `i8`（两阶段重打分）。
    pub quantization: Qualification,
    /// HNSW 建图精度档：`hybrid`（默认，i8 临时码流遍历 + f32 精排）或 `f32`。
    pub build_precision: Precision,
    /// 建图/建段工程调参覆盖（`None` = 库默认；仅 Mneme）。
    pub tuning: EngineTuning,
    /// 纯内存库（无路径、无 WAL/段；`mneme_mem` 别名）。
    pub in_memory: bool,
}

/// Mneme 建图/建段工程调参覆盖（A/B 用；`None` = 保持库默认）。
#[derive(Debug, Clone, Copy, Default)]
pub struct EngineTuning {
    /// `Tuning.hnsw_batch_rows`。
    pub hnsw_batch_rows: Option<usize>,
    /// `Tuning.hnsw_serial_rows`。
    pub hnsw_serial_rows: Option<usize>,
    /// `Tuning.hnsw_threads_max`。
    pub hnsw_threads_max: Option<usize>,
    /// `Tuning.flush_threads`。
    pub flush_threads: Option<usize>,
}

impl EngineTuning {
    /// 是否全部保持库默认。
    pub fn is_default(&self) -> bool {
        self.hnsw_batch_rows.is_none()
            && self.hnsw_serial_rows.is_none()
            && self.hnsw_threads_max.is_none()
            && self.flush_threads.is_none()
    }

    /// 渲染为报告备注（`key=value, ...`）。
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(value) = self.hnsw_batch_rows {
            parts.push(format!("hnsw_batch_rows={value}"));
        }
        if let Some(value) = self.hnsw_serial_rows {
            parts.push(format!("hnsw_serial_rows={value}"));
        }
        if let Some(value) = self.hnsw_threads_max {
            parts.push(format!("hnsw_threads_max={value}"));
        }
        if let Some(value) = self.flush_threads {
            parts.push(format!("flush_threads={value}"));
        }
        parts.join(", ")
    }
}

impl BuildOptions {
    /// 是否为默认配置（报告备注用）。
    pub fn is_default(&self) -> bool {
        matches!(self.quantization, Qualification::F32)
            && matches!(self.build_precision, Precision::Hybrid)
            && self.tuning.is_default()
            && !self.in_memory
    }

    /// 量化口径名（写进报告）。
    pub fn quantization_name(&self) -> &'static str {
        match self.quantization {
            Qualification::F32 => "f32",
            Qualification::I8 => "i8",
        }
    }

    /// 建图精度口径名（写进报告）。
    pub fn precision_name(&self) -> &'static str {
        match self.build_precision {
            Precision::F32 => "f32",
            Precision::Hybrid => "hybrid",
        }
    }
}

/// 段量化副本格式（CLI `--quant`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Qualification {
    /// 纯 f32 原向量。
    #[default]
    F32,
    /// i8 量化副本 + 两阶段精排。
    I8,
}

/// HNSW 建图精度档（CLI `--build-precision`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Precision {
    /// 全 f32 精确建图。
    F32,
    /// 默认：i8 临时码流遍历 + f32 精排选邻。
    #[default]
    Hybrid,
}

/// 引擎统一接口。
///
/// 要求 `Send + Sync`：查询测量支持多线程并发调用同一索引。
pub trait Engine: Send + Sync {
    /// 构建索引；`vectors` 为 row-major（长度 `rows * dim`）。
    ///
    /// # Arguments
    /// * `rows` - 向量行数。
    /// * `dim` - 向量维度。
    /// * `vectors` - 库内向量（row-major）。
    /// * `threads` - 构建并行度（≥ 1）。
    /// * `efs` - 计划查询的 ef 档位（仅 `instant-distance` 这类构建期锁定 ef 的
    ///   引擎需要参考；其余引擎可忽略）。
    /// * `options` - Mneme 专属构建选项；其余引擎忽略。
    ///
    /// # Errors
    /// 底层引擎创建或写入失败时返回错误说明。
    fn build(
        rows: usize,
        dim: usize,
        vectors: &[f32],
        threads: usize,
        efs: &[usize],
        options: BuildOptions,
    ) -> Result<Self, String>
    where
        Self: Sized;

    /// 引擎名（与 CLI `--engines` 同口径）。
    fn name(&self) -> &'static str;

    /// 引擎版本（人工与 `Cargo.toml` 同步）。
    fn version(&self) -> &'static str;

    /// 在指定 ef 下检索 top-`k`，返回数据集行号。
    ///
    /// # Errors
    /// 查询失败或 ef 不受支持时返回错误说明。
    fn search(&self, query: &[f32], k: usize, ef: usize) -> Result<Vec<u32>, String>;

    /// 该引擎是否支持运行时切换到此 ef（缺省全支持）。
    fn supports_ef(&self, _ef: usize) -> bool {
        true
    }

    /// 引擎自报的索引内存（如有；口径由各库定义，仅作参考）。
    fn index_bytes(&self) -> Option<u64> {
        None
    }

    /// 构建分段耗时（仅 Mneme 报告 `insert_batch` 与 `flush` 两段；缺省 `None`）。
    fn build_phases(&self) -> Option<crate::report::BuildPhases> {
        None
    }
}

/// 按名字构建引擎。
///
/// # Errors
/// 名字未知或引擎构建失败时返回错误说明。
pub fn build_engine(
    name: &str,
    rows: usize,
    dim: usize,
    vectors: &[f32],
    threads: usize,
    efs: &[usize],
    options: BuildOptions,
) -> Result<Box<dyn Engine>, String> {
    match name {
        "mneme" => Ok(Box::new(mneme::MnemeEngine::build(
            rows, dim, vectors, threads, efs, options,
        )?)),
        "mneme_i8" => {
            let options = BuildOptions {
                quantization: Qualification::I8,
                ..options
            };
            Ok(Box::new(mneme::MnemeEngine::build(
                rows, dim, vectors, threads, efs, options,
            )?))
        }
        "mneme_mem" => {
            let options = BuildOptions {
                in_memory: true,
                ..options
            };
            Ok(Box::new(mneme::MnemeEngine::build(
                rows, dim, vectors, threads, efs, options,
            )?))
        }
        "usearch" => Ok(Box::new(usearch::UsearchEngine::build(
            rows, dim, vectors, threads, efs, options,
        )?)),
        "hnsw_rs" => Ok(Box::new(hnswrs::HnswRsEngine::build(
            rows, dim, vectors, threads, efs, options,
        )?)),
        "hnsw_stable" => Ok(Box::new(hnswlib::HnswStableEngine::build(
            rows, dim, vectors, threads, efs, options,
        )?)),
        "instant_distance" => Ok(Box::new(instant::InstantEngine::build(
            rows, dim, vectors, threads, efs, options,
        )?)),
        other => Err(format!("未知引擎 `{other}`")),
    }
}

/// 通用并行分块：把 `[0, rows)` 均分给至多 `threads` 个工作线程执行 `body`。
///
/// 用于无内置并行插入 API 的引擎（usearch / hnsw-stable），使其与
/// `mneme`（批内并行）、`hnsw_rs`（`parallel_insert`）的构建条件对等。
pub(crate) fn for_each_chunk_parallel<F>(rows: usize, threads: usize, body: F) -> Result<(), String>
where
    F: Fn(usize, usize) -> Result<(), String> + Copy + Send,
{
    let threads = threads.clamp(1, rows.max(1));
    if threads == 1 {
        return body(0, rows);
    }
    let chunk = rows.div_ceil(threads);
    std::thread::scope(|scope| -> Result<(), String> {
        let mut handles = Vec::new();
        for worker in 0..threads {
            let start = worker * chunk;
            let end = rows.min(start + chunk);
            if start >= end {
                break;
            }
            handles.push(scope.spawn(move || body(start, end)));
        }
        let mut failure: Option<String> = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    failure.get_or_insert(error);
                }
                Err(_) => {
                    failure.get_or_insert_with(|| "构建线程 panic".to_string());
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_chunks_cover_all_rows_exactly_once() {
        let hits = std::sync::Mutex::new(vec![0_u8; 100]);
        for_each_chunk_parallel(100, 4, |start, end| {
            let mut guard = hits.lock().map_err(|e| e.to_string())?;
            for slot in guard.iter_mut().take(end).skip(start) {
                *slot += 1;
            }
            Ok(())
        })
        .expect("并行分块成功");
        let guard = hits.lock().expect("锁未被毒化");
        assert!(guard.iter().all(|count| *count == 1), "每行恰好遍历一次");
    }

    #[test]
    fn parallel_chunks_propagate_first_error() {
        let error = for_each_chunk_parallel(10, 2, |start, _end| {
            if start == 0 {
                Err("首块失败".to_string())
            } else {
                Ok(())
            }
        })
        .expect_err("应向上传播失败");
        assert_eq!(error, "首块失败");
    }
}
