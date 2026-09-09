//! 时间源:统一注入时钟以支持确定性测试。
//!
//! TTL、遗忘曲线与 `touch` 一律经 [`Clock`] 取时;生产用 [`SystemClock`],
//! 测试注入可回拨 / 快进的假时钟。

/// 时间源:TTL / 遗忘曲线 / `touch` 一律经此取"当前 Unix 毫秒"。
///
/// 生产用 [`SystemClock`];测试注入可回拨 / 快进的假时钟,保证确定性。
pub trait Clock: Send + Sync {
    /// 返回当前 Unix 毫秒时间戳。
    ///
    /// # Returns
    ///
    /// 与 Unix epoch 同基准的毫秒数;epoch 之前为负值。
    fn now_unix_ms(&self) -> i64;
}

/// 读取系统墙上时钟的时间源。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> i64 {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(elapsed) => elapsed.as_millis() as i64,
            Err(before_epoch) => -(before_epoch.duration().as_millis() as i64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_returns_positive_unix_ms() {
        // 被测对象本身就是墙上时钟,无法注入假时钟;断言仅验证量纲与量级
        // (2020-01-01 之后),不追求精确时刻。
        let now = SystemClock.now_unix_ms();
        assert!(now > 1_577_836_800_000);
    }
}
