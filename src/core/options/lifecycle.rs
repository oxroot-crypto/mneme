//! 后台 compaction 策略(L5)。
//!
//! 分级合并触发条件与历史版本保留窗口;语义见设计 07 §4.2 与 07 §4.2a。

use std::time::Duration;

/// 后台 compaction 策略(L5)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactionPolicy {
    /// 分级比 r,默认 4。
    pub tier_ratio: u32,
    /// 同层合并阈值 T,默认 4。
    pub tier_count: u32,
    /// 墓碑 + 过期占比触发线,默认 0.25。
    pub dead_ratio: f32,
    /// WAL 压力触发线(字节),默认 256 MiB。
    pub wal_bytes: u64,
    /// 单个 WAL 文件轮转阈值(字节),默认 64 MiB。
    pub wal_file_bytes: u64,
    /// 段初始目标行数 B,默认 8192。
    pub segment_rows: u64,
    /// 后台合并磁盘配额,默认 0.30。
    pub io_budget: f32,
    /// 历史版本保留窗口;`None` 表示永久保留(默认)。
    pub history_horizon: Option<Duration>,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            tier_ratio: 4,
            tier_count: 4,
            dead_ratio: 0.25,
            wal_bytes: 256 * 1024 * 1024,
            wal_file_bytes: 64 * 1024 * 1024,
            segment_rows: 8192,
            io_budget: 0.30,
            history_horizon: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_defaults_match_design() {
        let compaction = CompactionPolicy::default();
        assert_eq!(compaction.tier_ratio, 4);
        assert_eq!(compaction.tier_count, 4);
        assert_eq!(compaction.dead_ratio, 0.25);
        assert_eq!(compaction.segment_rows, 8192);
        assert_eq!(compaction.io_budget, 0.30);
        assert_eq!(compaction.history_horizon, None);
    }
}
