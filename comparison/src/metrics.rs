//! 延迟统计与进程资源采集。
//!
//! 计时口径：查询延迟为 `Instant` 墙钟时间，单条采样；分位数用全部样本排序后
//! 取最近秩（nearest-rank）；QPS 按均值折算（单线程时即单线程吞吐）。
//! 内存口径：读 `/proc/self/status` 的 `VmRSS`（Linux 专用）；基准子进程每引擎
//! 独立运行，互不污染。

/// 一组延迟样本的汇总统计（微秒口径）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LatencyStats {
    /// 样本数（查询条数）。
    pub samples: usize,
    /// 平均延迟（微秒）。
    pub mean_us: f64,
    /// 最小延迟（微秒）。
    pub min_us: f64,
    /// 中位延迟（微秒）。
    pub p50_us: f64,
    /// P90 延迟（微秒）。
    pub p90_us: f64,
    /// P95 延迟（微秒）。
    pub p95_us: f64,
    /// P99 延迟（微秒）。
    pub p99_us: f64,
    /// 最大延迟（微秒）。
    pub max_us: f64,
    /// 按均值折算的每秒查询数（样本连续测量的单线程口径）。
    pub qps: f64,
}

/// 汇总延迟样本（纳秒），样本为空时返回 `None`。
///
/// # Arguments
/// * `samples_ns` - 每条查询的耗时（纳秒）。
pub fn summarize(mut samples_ns: Vec<u64>) -> Option<LatencyStats> {
    if samples_ns.is_empty() {
        return None;
    }
    samples_ns.sort_unstable();
    let samples = samples_ns.len();
    let mean_ns = samples_ns.iter().sum::<u64>() as f64 / samples as f64;
    let pick = |quantile: f64| -> f64 {
        // 最近秩：ceil(q * n) - 1，夹到 [0, n-1]。
        let rank = (quantile * samples as f64).ceil() as usize;
        samples_ns[rank.saturating_sub(1).min(samples - 1)] as f64 / 1_000.0
    };
    Some(LatencyStats {
        samples,
        mean_us: mean_ns / 1_000.0,
        min_us: samples_ns[0] as f64 / 1_000.0,
        p50_us: pick(0.50),
        p90_us: pick(0.90),
        p95_us: pick(0.95),
        p99_us: pick(0.99),
        max_us: samples_ns[samples - 1] as f64 / 1_000.0,
        qps: if mean_ns > 0.0 {
            1e9 / mean_ns
        } else {
            f64::INFINITY
        },
    })
}

/// 读取当前进程常驻内存（VmRSS，字节）；非 Linux 或读取失败返回 `None`。
pub fn rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// CPU 型号（读 `/proc/cpuinfo`；读不到返回 `unknown`）。
pub fn cpu_model() -> String {
    let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") else {
        return "unknown".to_string();
    };
    cpuinfo
        .lines()
        .find_map(|line| line.strip_prefix("model name").map(str::trim))
        .map(|rest| rest.trim_start_matches(':').trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// 主机名（读 `/proc/sys/kernel/hostname`；读不到返回 `unknown`）。
pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// 可用并行度（核数）。
pub fn parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_picks_nearest_rank() {
        let stats = summarize(vec![1_000, 2_000, 3_000, 4_000]).expect("非空");
        assert_eq!(stats.samples, 4);
        assert!((stats.p50_us - 2.0).abs() < 1e-9, "p50 为第 2 个样本");
        assert!((stats.p95_us - 4.0).abs() < 1e-9);
        assert!((stats.mean_us - 2.5).abs() < 1e-9);
        assert!((stats.qps - 400_000.0).abs() < 1e-6);
    }

    #[test]
    fn summarize_rejects_empty() {
        assert!(summarize(Vec::new()).is_none());
    }

    #[test]
    fn rss_is_plausible_on_linux() {
        if cfg!(target_os = "linux") {
            let rss = rss_bytes().expect("Linux 下可读 VmRSS");
            assert!(rss > 0);
        }
    }
}
