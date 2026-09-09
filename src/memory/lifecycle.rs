//! 遗忘策略与报告(`lifecycle.rs`)。
//!
//! L1 提供内存版指数遗忘:保留分随「距最近有效时间」按半衰期指数衰减,
//! 低于 `min_importance` 且未被 `protect` 白名单豁免的记录被墓碑。
//! 完整曲线与后台调度见设计 07;自动遗忘默认关闭(`Builder::retention` 缺省 `None`)。

use std::time::Duration;

use crate::core::types::RowId;
use crate::memory::pred::Expr;

/// 遗忘曲线缺省半衰期(天)。
const DEFAULT_HALF_LIFE_DAYS: u64 = 14;

/// 保留分缺省下限。
const DEFAULT_MIN_IMPORTANCE: f32 = 0.2;

/// 访问增益缺省权重。
const DEFAULT_ACCESS_WEIGHT: f32 = 0.05;

/// 一天的秒数。
const SECS_PER_DAY: u64 = 24 * 60 * 60;

/// 遗忘策略(写入期/后台共用)。
#[derive(Debug, Clone)]
pub struct Retention {
    /// 遗忘曲线半衰期,默认 14 天。
    pub half_life: Duration,
    /// 保留分下限,低于则候选遗忘,默认 0.2。
    pub min_importance: f32,
    /// 访问增益权重,默认 0.05。
    pub w: f32,
    /// 白名单:命中者豁免遗忘。
    pub protect: Option<Expr>,
}

impl Retention {
    /// 返回默认策略(半衰期 14 天、下限 0.2、增益权重 0.05)。
    ///
    /// # Examples
    /// ```
    /// use mneme::Retention;
    /// let policy = Retention::new();
    /// assert!(policy.half_life.as_secs() > 0);
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置半衰期。
    pub fn half_life(mut self, half_life: Duration) -> Self {
        self.half_life = half_life;
        self
    }

    /// 设置保留分下限。
    pub fn min_importance(mut self, min_importance: f32) -> Self {
        self.min_importance = min_importance;
        self
    }

    /// 设置访问增益权重。
    pub fn w(mut self, w: f32) -> Self {
        self.w = w;
        self
    }

    /// 设置保护过滤器。
    pub fn protect(mut self, protect: Expr) -> Self {
        self.protect = Some(protect);
        self
    }
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            half_life: Duration::from_secs(DEFAULT_HALF_LIFE_DAYS * SECS_PER_DAY),
            min_importance: DEFAULT_MIN_IMPORTANCE,
            w: DEFAULT_ACCESS_WEIGHT,
            protect: None,
        }
    }
}

/// `retain` 的执行报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetainReport {
    /// 扫描的活记录数。
    pub scanned: usize,
    /// 实际墓碑的记录数。
    pub forgotten: usize,
    /// 被遗忘记录的 `RowId`(可审计,I23)。
    pub sampled_ids: Vec<RowId>,
}

/// 计算一条记录的保留分:$\text{imp} \cdot 2^{-t/T_{1/2}} + w \cdot \ln(1+c)$。
///
/// # Arguments
/// * `importance` - 重要度,`[0,1]`。
/// * `age_ms` - 距最近有效时间的毫秒数(负值按 0 处理)。
/// * `half_life` - 半衰期;为 0 时衰减系数按 0 处理。
/// * `w` - 访问增益权重。
/// * `access_count` - 累计访问次数。
pub(crate) fn retention_score(
    importance: f32,
    age_ms: i64,
    half_life: Duration,
    w: f32,
    access_count: u32,
) -> f32 {
    let half_life_ms = half_life.as_millis() as f64;
    let decay = if half_life_ms <= 0.0 {
        0.0
    } else {
        2.0_f64.powf(-(age_ms.max(0) as f64) / half_life_ms)
    };
    let access_gain = f64::from(w) * (1.0 + f64::from(access_count)).ln();
    (f64::from(importance) * decay + access_gain) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FC-LIFE-POST-002
    #[test]
    fn retention_score_formula() {
        let half_life = Duration::from_secs(14 * 24 * 60 * 60);
        let half_life_ms = half_life.as_millis() as i64;
        // 无访问、无衰减(w=0, age=0)→ 等于 importance
        assert!((retention_score(0.8, 0, half_life, 0.0, 0) - 0.8).abs() < 1e-6);
        // 恰好一个半衰期 → 衰减为一半
        assert!((retention_score(1.0, half_life_ms, half_life, 0.0, 0) - 0.5).abs() < 1e-4);
        // 负 age 按 0 处理
        assert!((retention_score(1.0, -1_000, half_life, 0.0, 0) - 1.0).abs() < 1e-6);
        // T½ = 0 → 衰减项为 0
        let zero_half = retention_score(1.0, 1_000, Duration::ZERO, 0.0, 0);
        assert!(zero_half.abs() < 1e-6, "got {zero_half}");
        // 访问增益 w·ln(1+c):w=1、c=1 → ln 2
        let with_access = retention_score(0.0, 0, half_life, 1.0, 1);
        assert!(
            (with_access - 2.0_f32.ln()).abs() < 1e-4,
            "got {with_access}"
        );
    }
}
