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
            let _ = handle.join();
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
    pub(crate) fn is_last_handle(&self) -> bool {
        Arc::strong_count(&self.stop) == 1
    }

    /// 等待被唤醒或超时。
    fn wait(&self, timeout: Duration) {
        let (lock, cvar) = &*self.stop;
        let guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = cvar.wait_timeout(guard, timeout);
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
    let mut last_access_ms = context.config.clock.now_unix_ms();
    let mut last_retain_ms = last_access_ms;
    let mut last_compact_ms = last_access_ms;
    let compact_interval = context
        .config
        .retain_interval
        .map_or(COMPACT_INTERVAL, |interval| interval.max(MIN_TICK));
    loop {
        if handle.is_stopped() {
            break;
        }
        // 强引用只在单轮工作内持有,随后立即释放并进入等待:
        // 库句柄全部释放后 `upgrade` 失败即退出,不阻止 Drop 与锁释放。
        {
            let Some(table) = context.table.upgrade() else {
                break;
            };
            let now_ms = context.config.clock.now_unix_ms();
            if elapsed(now_ms, last_access_ms) >= context.config.access_flush_interval {
                table.flush_access(now_ms);
                last_access_ms = now_ms;
            }
            if let Some(policy) = &context.config.retention {
                let interval = context
                    .config
                    .retain_interval
                    .unwrap_or(policy.half_life / 4);
                if elapsed(now_ms, last_retain_ms) >= interval {
                    run_retain(&table, &context.config, policy);
                    last_retain_ms = now_ms;
                }
            }
            if !context.control.is_paused()
                && elapsed(now_ms, last_compact_ms) >= compact_interval
                && let Some(store) = context.store.as_ref().and_then(Weak::upgrade)
            {
                let mneme = Mneme {
                    table: Arc::clone(&table),
                    config: Arc::clone(&context.config),
                    control: context.control.clone(),
                    store: Some(store),
                    maintenance: None,
                };
                // reason: 后台 compaction 为尽力而为;失败由下一轮重试,已提交状态不变
                // (FC-LIFE-ERR-001),不影响前台读写的正确性。
                let _ = mneme.compact();
                last_compact_ms = now_ms;
            }
        }
        handle.wait(tick_interval(&context.config));
    }
}

/// 对全部已注册命名空间执行一轮自动遗忘并记录聚合报告(I23 审计)。
fn run_retain(table: &Arc<Table>, config: &Arc<Config>, policy: &Retention) {
    let paths: Vec<Arc<str>> = table.view().ns_registry.values().cloned().collect();
    let mut report = RetainReport::default();
    for path in paths {
        let namespace = Namespace {
            table: Arc::clone(table),
            config: Arc::clone(config),
            ns_path: path,
        };
        if let Ok(partial) = namespace.retain(policy.clone()) {
            report.scanned += partial.scanned;
            report.forgotten += partial.forgotten;
            report.sampled_ids.extend(partial.sampled_ids);
        }
    }
    report.sampled_ids.truncate(MAX_SAMPLED_IDS);
    table.set_retain_report(report);
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
