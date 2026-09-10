//! 时间源:统一注入时钟以支持确定性测试。
//!
//! TTL、遗忘曲线与 `touch` 一律经 [`Clock`] 取时;生产用 [`SystemClock`],
//! 测试注入可回拨 / 快进的假时钟。[`MonotonicClock`] 在任意时源之上做单调钳制,
//! 使时钟回拨不倒退(FC-GLOBAL-PRE-005,设计 04 §10.2)。

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

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

/// 单调钳制时钟:在任意底层 [`Clock`] 之上返回**历史最大值**。
///
/// 系统时钟回拨(如 NTP 校正)时,时间戳不倒退——TTL 因此**只可能晚消失**,
/// 不会早消失、也不会让已过期记录复活(FC-GLOBAL-PRE-005,设计 04 §10.2)。
pub(crate) struct MonotonicClock {
    inner: Arc<dyn Clock>,
    last: AtomicI64,
}

impl MonotonicClock {
    /// 以 `inner` 为底层时源新建;首次调用以 `inner` 当前值为准。
    pub(crate) fn new(inner: Arc<dyn Clock>) -> Self {
        Self {
            inner,
            last: AtomicI64::new(i64::MIN),
        }
    }
}

impl Clock for MonotonicClock {
    fn now_unix_ms(&self) -> i64 {
        let now = self.inner.now_unix_ms();
        let mut last = self.last.load(Ordering::Relaxed);
        // CAS 循环:返回观测到的最大值;回拨时返回历史最大值。
        while now > last {
            match self
                .last
                .compare_exchange_weak(last, now, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return now,
                Err(observed) => last = observed,
            }
        }
        last
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
