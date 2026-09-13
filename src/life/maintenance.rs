//! 后台维护线程:访问统计攒批、自动遗忘与自动 compaction(设计 07 §2/§3.4/§4.4)。
//!
//! 单线程按最短节拍醒来,分别按各自周期执行三件事:
//!
//! - **访问攒批**:把读路径命中缓冲合并进写状态并落 WAL(`access_flush_interval`,默认 30s);
//! - **自动遗忘**:`retention` 配置开启后按 `retain_interval`(默认半衰期/4)扫描全部命名空间;
//! - **自动 compaction**:持久库按 tier/死比率触发(`pause()` 时跳过)。
//!
//! 线程只持 `Weak<Table>`/`Weak<Store>`:库句柄全部释放后线程自行退出,不阻止 Drop;
//! `close()` 经 [`MaintenanceHandle::stop`] 同步请求退出。所有时间取注入 `Clock`,
//! 测试可用假时钟确定性推进。

use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use crate::core::error::{MnemeError, Result};
use crate::memory::config::Config;
use crate::memory::ops::CompactionControl;
use crate::memory::table::Table;
use crate::memory::{Mneme, Namespace, RetainReport, Retention};
use crate::persist::store::Store;

/// 维护线程最短/最长唤醒间隔(时钟未推进时也周期醒来检查)。
const MIN_TICK: Duration = Duration::from_millis(10);
const MAX_TICK: Duration = Duration::from_millis(500);

/// 自动 compaction 的默认检查周期(生产约 1 次/秒;测试可用短 `retain_interval` 加速)。
const COMPACT_INTERVAL: Duration = Duration::from_secs(1);

/// 单次自动遗忘报告保留的抽样上限(聚合多个命名空间后截断)。
const MAX_SAMPLED_IDS: usize = 1024;

/// 自动遗忘默认扫描周期相对半衰期的分母(半衰期 / 4,设计 07 §3.4)。
const RETAIN_INTERVAL_DIVISOR: u32 = 4;

/// 后台维护句柄:停止信号 + 唤醒条件变量 + 线程 join 句柄(克隆共享)。
#[derive(Clone)]
pub(crate) struct MaintenanceHandle {
    stop: Arc<(Mutex<bool>, Condvar)>,
    join: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
}

impl MaintenanceHandle {
    /// 请求维护线程退出并等待其结束(幂等);重复调用不再 join。
    pub(crate) fn stop(&self) {
        {
            let (lock, cvar) = &*self.stop;
            *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
            cvar.notify_all();
        }
        let handle = self
            .join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = handle {
            // reason: 维护线程内部的 panic 由 `run` 隔离(线程异常终止不代表库损坏),
            // join 结果仅用于等待退出,无可执行的恢复动作。
            let _ = handle.join().ok();
        }
    }

    /// 是否已被请求停止。
    fn is_stopped(&self) -> bool {
        *self
            .stop
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 是否为最后一个维护句柄(所有 `Mneme` 克隆已释放)。
    ///
    /// 运行期 `stop` 至少有 2 个强引用:本句柄 + 维护线程内 `run` 持有的句柄;
    /// 线程退出后降为 1。故 `<= 2` 表示"本句柄是最后一个库侧句柄"。
    pub(crate) fn is_last_handle(&self) -> bool {
        Arc::strong_count(&self.stop) <= 2
    }

    /// 等待被唤醒或超时。
    fn wait(&self, timeout: Duration) {
        let (lock, cvar) = &*self.stop;
        let guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // reason: 超时返回属正常路径;锁 poison 已在上一行恢复,无需再检查返回值。
        let _ = cvar.wait_timeout(guard, timeout).ok();
    }
}

/// 维护线程上下文(全部 `Weak`,不阻止库 Drop)。
struct MaintenanceContext {
    table: Weak<Table>,
    config: Arc<Config>,
    control: CompactionControl,
    store: Option<Weak<Store>>,
}

/// 启动后台维护线程并返回停止句柄。
pub(crate) fn spawn(
    table: &Arc<Table>,
    config: &Arc<Config>,
    control: &CompactionControl,
    store: Option<&Arc<Store>>,
) -> MaintenanceHandle {
    let context = MaintenanceContext {
        table: Arc::downgrade(table),
        config: Arc::clone(config),
        control: control.clone(),
        store: store.map(Arc::downgrade),
    };
    let stop = Arc::new((Mutex::new(false), Condvar::new()));
    let handle = MaintenanceHandle {
        stop: Arc::clone(&stop),
        join: Arc::new(Mutex::new(None)),
    };
    // reason: 线程创建失败(资源耗尽)时维护退化为「仅显式调用」,不影响正确性。
    let join = std::thread::Builder::new()
        .name("mneme-maintenance".to_string())
        .spawn(move || run(context, stop));
    if let Ok(join) = join {
        *handle
            .join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(join);
    }
    handle
}

/// 维护线程主循环。
fn run(context: MaintenanceContext, stop: Arc<(Mutex<bool>, Condvar)>) {
    let handle = MaintenanceHandle {
        stop,
        join: Arc::new(Mutex::new(None)),
    };
    let mut timers = Timers::new(context.config.clock.now_unix_ms());
    let compact_interval = context
        .config
        .retain_interval
        .map_or(COMPACT_INTERVAL, |interval| interval.max(MIN_TICK));
    loop {
        if handle.is_stopped() {
            break;
        }
        if !run_tick(&context, &mut timers, compact_interval) {
            break;
        }
        handle.wait(tick_interval(&context.config));
    }
}

/// 三个维护周期的上次执行时刻(Unix 毫秒)。
struct Timers {
    /// 访问攒批上次落盘时刻。
    access_ms: i64,
    /// 自动遗忘上次扫描时刻。
    retain_ms: i64,
    /// 自动 compaction 上次检查时刻。
    compact_ms: i64,
}

impl Timers {
    /// 以当前时刻初始化全部计时器。
    fn new(now_ms: i64) -> Self {
        Self {
            access_ms: now_ms,
            retain_ms: now_ms,
            compact_ms: now_ms,
        }
    }
}

/// 执行一轮维护工作;弱引用失效(库已释放)时返回 `false` 请求退出。
///
/// 强引用只在单轮工作内持有,随后立即释放并进入等待:库句柄全部释放后
/// `upgrade` 失败即退出,不阻止 Drop 与锁释放。
fn run_tick(context: &MaintenanceContext, timers: &mut Timers, compact_interval: Duration) -> bool {
    let Some(table) = context.table.upgrade() else {
        return false;
    };
    let now_ms = context.config.clock.now_unix_ms();
    if elapsed(now_ms, timers.access_ms) >= context.config.access_flush_interval {
        table.flush_access(now_ms);
        timers.access_ms = now_ms;
    }
    if let Some(policy) = &context.config.retention {
        let interval = context
            .config
            .retain_interval
            .unwrap_or(policy.half_life / RETAIN_INTERVAL_DIVISOR);
        if elapsed(now_ms, timers.retain_ms) >= interval {
            // reason: 后台维护尽力而为;retain 失败由下一轮重试,不影响前台读写正确性。
            let _ = run_retain(&table, &context.config, policy).ok();
            timers.retain_ms = now_ms;
        }
    }
    if !context.config.read_only
        && !context.control.is_paused()
        && elapsed(now_ms, timers.compact_ms) >= compact_interval
        && let Some(store) = context.store.as_ref().and_then(Weak::upgrade)
    {
        let mneme = Mneme {
            table,
            config: Arc::clone(&context.config),
            control: context.control.clone(),
            store: Some(store),
            maintenance: None,
        };
        // reason: 后台 compaction 为尽力而为;失败由下一轮重试,已提交状态不变
        // (FC-LIFE-ERR-001),不影响前台读写的正确性。
        let _ = mneme.compact().ok();
        timers.compact_ms = now_ms;
    }
    true
}

impl Mneme {
    /// 手动执行一轮后台维护(访问攒批、自动遗忘、自动 compaction)。
    ///
    /// 与后台维护线程的单轮逻辑一致(不受周期阈值约束,三项无条件执行);用于
    /// 没有后台线程、线程未及唤醒或测试需要确定性推进的场景。纯内存库只执行
    /// 访问攒批与遗忘;只读库跳过 compaction;`compact` 失败按显式调用口径
    /// 向上传播。
    ///
    /// # Returns
    /// 完成一轮维护返回 `()`;无触发条件时同样成功。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`];遗忘或 compaction I/O/编码失败时
    /// 返回结构化错误(与显式 [`Mneme::compact`] 同口径)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    /// let db = Mneme::in_memory(2).unwrap();
    /// db.namespace("demo")
    ///     .insert(Record::new(vec![1.0, 0.0]).key("a"))
    ///     .unwrap();
    /// db.maintenance_tick().unwrap();
    /// ```
    pub fn maintenance_tick(&self) -> Result<()> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        drop(view);
        let now_ms = self.config.clock.now_unix_ms();
        self.table.flush_access(now_ms);
        if let Some(policy) = &self.config.retention {
            run_retain(&self.table, &self.config, policy)?;
        }
        // 只读库无写权限,跳过 compaction(与后台线程同口径)。
        if self.store.is_some() && !self.config.read_only && !self.control.is_paused() {
            self.compact()?;
        }
        Ok(())
    }
}

/// 对全部已注册命名空间执行一轮自动遗忘并记录聚合报告(I23 审计)。
///
/// # Errors
/// 任一命名空间的 `retain` 失败(参数非法/已关闭/I/O)时返回结构化错误,
/// 绝不静默吞掉(`maintenance_tick` 直接传播;后台线程按尽力而为重试)。
fn run_retain(table: &Arc<Table>, config: &Arc<Config>, policy: &Retention) -> Result<()> {
    let paths: Vec<Arc<str>> = table.view().ns_registry.values().cloned().collect();
    let mut report = RetainReport::default();
    for path in paths {
        let namespace = Namespace {
            table: Arc::clone(table),
            config: Arc::clone(config),
            ns_path: path,
        };
        let partial = namespace.retain(policy.clone())?;
        report.scanned += partial.scanned;
        report.forgotten += partial.forgotten;
        report.sampled_ids.extend(partial.sampled_ids);
    }
    report.sampled_ids.truncate(MAX_SAMPLED_IDS);
    table.set_retain_report(report);
    Ok(())
}

/// 两次维护之间的实际经过时长(毫秒,负值按 0)。
fn elapsed(now_ms: i64, last_ms: i64) -> Duration {
    let delta = now_ms.saturating_sub(last_ms).max(0);
    Duration::from_millis(u64::try_from(delta).unwrap_or(u64::MAX))
}

/// 维护线程唤醒间隔:取访问周期与遗忘周期中较小者,夹紧到 `[MIN_TICK, MAX_TICK]`。
fn tick_interval(config: &Config) -> Duration {
    let mut tick = config.access_flush_interval.min(MAX_TICK);
    if let Some(interval) = config.retain_interval {
        tick = tick.min(interval);
    }
    tick.max(MIN_TICK)
}
