//! 后台维护、遗忘、观测与调参 setter。

use std::sync::Arc;
use std::time::Duration;

use crate::core::options::{Clock, CompactionPolicy, Limits, Tuning};
use crate::memory::lifecycle::Retention;

use super::super::Builder;

impl Builder {
    /// 是否启动后台维护线程(默认 `true`;`FC-LIFE-POST-010`)。
    ///
    /// `false` 时不自动 compaction、不自动遗忘、不周期落访问统计——适合批量导入/
    /// 建库期"先闸住维护、建完统一整理"的场景(避免维护与导入争抢 CPU/IO)。
    /// 手动 [`Mneme::maintenance_tick`](crate::memory::Mneme::maintenance_tick)/
    /// [`Mneme::compact`](crate::memory::Mneme::compact)/
    /// [`Namespace::retain`](crate::memory::Namespace::retain) 不受影响;只读实例的
    /// MANIFEST 探测线程与本开关无关。
    ///
    /// # Arguments
    ///
    /// * `enabled` - `true` 启动后台维护线程;`false` 不启动。
    ///
    /// # Returns
    ///
    /// 携带维护开关的构建器(链式)。
    ///
    /// # Examples
    /// ```
    /// use mneme::Builder;
    /// let db = Builder::default()
    ///     .dimension(2)
    ///     .maintenance(false)
    ///     .build()
    ///     .unwrap();
    /// let _ = db.namespace("demo");
    /// ```
    pub fn maintenance(mut self, enabled: bool) -> Self {
        self.maintenance = enabled;
        self
    }

    /// 设置 compaction 策略(L5 生效)。
    ///
    /// # Arguments
    ///
    /// * `compaction` - 后台合并策略。
    ///
    /// # Returns
    ///
    /// 携带 compaction 策略的构建器(链式)。
    pub fn compaction(mut self, compaction: CompactionPolicy) -> Self {
        self.compaction = compaction;
        self
    }

    /// 开启/关闭后台自动遗忘(默认 `None` = 关闭)。
    ///
    /// # Arguments
    ///
    /// * `retention` - 遗忘策略;`None` = 关闭后台自动遗忘。
    ///
    /// # Returns
    ///
    /// 携带遗忘策略的构建器(链式)。
    pub fn retention(mut self, retention: Option<Retention>) -> Self {
        self.retention = retention;
        self
    }

    /// 设置后台遗忘扫描周期(L5 生效)。
    ///
    /// # Arguments
    ///
    /// * `interval` - 扫描周期。
    ///
    /// # Returns
    ///
    /// 携带扫描周期的构建器(链式)。
    pub fn retain_interval(mut self, interval: Duration) -> Self {
        self.retain_interval = Some(interval);
        self
    }

    /// 设置访问统计落盘周期(L5 生效)。
    ///
    /// # Arguments
    ///
    /// * `interval` - 落盘周期。
    ///
    /// # Returns
    ///
    /// 携带落盘周期的构建器(链式)。
    pub fn access_flush_interval(mut self, interval: Duration) -> Self {
        self.access_flush_interval = interval;
        self
    }

    /// 设置事件可观测钩子(默认无;设计 12 §4)。
    ///
    /// # Arguments
    ///
    /// * `observer` - 事件回调;回调 panic 被隔离,不影响引擎行为(I30)。
    ///
    /// # Returns
    ///
    /// 携带观察者的构建器(链式)。
    pub fn observer(mut self, observer: std::sync::Arc<dyn crate::Observer>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// 设置并行度;`0` = 自动。
    ///
    /// # Arguments
    ///
    /// * `parallelism` - 并行扫描线程数;`0` 表示自动探测。
    ///
    /// # Returns
    ///
    /// 携带并行度的构建器(链式)。
    pub fn parallelism(mut self, parallelism: usize) -> Self {
        self.parallelism = parallelism;
        self
    }

    /// 设置进阶调参。
    ///
    /// # Arguments
    ///
    /// * `tuning` - 暴力扫描分块/字典上限/布隆参数等进阶项。
    ///
    /// # Returns
    ///
    /// 携带调参的构建器(链式)。
    pub fn tuning(mut self, tuning: Tuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// 设置数据限额。
    ///
    /// # Arguments
    ///
    /// * `limits` - key/text/metadata 等限额。
    ///
    /// # Returns
    ///
    /// 携带限额的构建器(链式)。
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// 注入时钟(测试确定性)。
    ///
    /// # Arguments
    ///
    /// * `clock` - 时间源;测试可注入可回拨/快进的假时钟。
    ///
    /// # Returns
    ///
    /// 携带时钟的构建器(链式)。
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}
