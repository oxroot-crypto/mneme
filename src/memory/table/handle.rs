//! 内存表句柄:写锁 / 读锁 / 写事务(`table/handle.rs`)。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::core::error::Result;
use crate::core::types::RowId;
use crate::memory::analysis::{BloomSet, ZoneIndex};
use crate::memory::config::Config;
use crate::memory::lifecycle::RetainReport;
use crate::memory::ops::Histogram;

use super::PersistHook;
use super::state::WriterState;
use super::view::ReaderView;

/// 访问缓冲条目数上限(约 100 万条):维护线程停止/panic 时缓冲不会无界增长;
/// 超限后新 `RowId` 被丢弃(只影响遗忘速度估计,FC-LIFE-POST-004)。
const MAX_ACCESS_BUFFER_ENTRIES: usize = 1 << 20;

/// 内存表:写状态 + 已发布读视图 + 配置 + 可选持久钩子。
pub(crate) struct Table {
    pub(crate) writer: Mutex<WriterState>,
    pub(crate) reader: RwLock<Arc<ReaderView>>,
    pub(crate) config: Arc<Config>,
    /// 持久层写日志钩子;`None` = 纯内存(L1)。
    pub(crate) persist: Option<Arc<dyn PersistHook>>,
    /// 读路径命中的访问计数缓冲(攒批后由后台维护合并进写状态并落 WAL)。
    pub(crate) access_buffer: Mutex<HashMap<RowId, u32>>,
    /// 最近一次后台自动遗忘报告(I23 可审计;未开启时 `None`)。
    pub(crate) retain_report: Mutex<Option<RetainReport>>,
    /// 查询延迟直方图(固定 32 桶;`execute()` 每次采样)。
    pub(crate) latency: Mutex<Histogram>,
}

impl Table {
    /// 以给定配置新建空表(无持久钩子)。
    pub(crate) fn new(config: Arc<Config>) -> Self {
        Self::new_with(config, None)
    }

    /// 以给定配置与持久钩子新建空表。
    pub(crate) fn new_with(config: Arc<Config>, persist: Option<Arc<dyn PersistHook>>) -> Self {
        let mut writer = WriterState::new();
        // 检索加速结构的容量/开关由建库配置决定(空状态上重建无数据损失)。
        writer.stopwords_enabled = config.tuning.stopwords;
        writer.index_fields_max = config.tuning.field_dict_max as usize;
        writer.bloom_fpp = config.tuning.bloom_fpp;
        writer.ns_depth_max = config.limits.ns_depth;
        writer.zones = Arc::new(ZoneIndex::new(config.tuning.field_dict_max as usize));
        writer.key_bloom = Arc::new(BloomSet::new(
            crate::memory::analysis::BLOOM_INITIAL_CAPACITY,
            config.tuning.bloom_fpp,
        ));
        let view = Arc::new(writer.snapshot());
        Self {
            writer: Mutex::new(writer),
            reader: RwLock::new(view),
            config,
            persist,
            access_buffer: Mutex::new(HashMap::new()),
            retain_report: Mutex::new(None),
            latency: Mutex::new(Histogram::default()),
        }
    }

    /// 以恢复出的写状态构造表(持久化打开;状态已含段与 WAL 重放结果)。
    pub(crate) fn from_state(
        config: Arc<Config>,
        mut state: WriterState,
        persist: Option<Arc<dyn PersistHook>>,
    ) -> Self {
        state.ns_depth_max = config.limits.ns_depth;
        let view = Arc::new(state.snapshot());
        Self {
            writer: Mutex::new(state),
            reader: RwLock::new(view),
            config,
            persist,
            access_buffer: Mutex::new(HashMap::new()),
            retain_report: Mutex::new(None),
            latency: Mutex::new(Histogram::default()),
        }
    }

    /// 获取写锁;锁中毒时恢复内部数据继续工作(不 panic)。
    pub(crate) fn write(&self) -> MutexGuard<'_, WriterState> {
        self.writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 获取读锁。
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, Arc<ReaderView>> {
        self.reader
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 克隆当前读视图。
    pub(crate) fn view(&self) -> Arc<ReaderView> {
        Arc::clone(&self.read())
    }

    /// 把写状态发布为新的读视图(调用方须持有写锁)。
    pub(crate) fn publish(&self, ws: &WriterState) {
        let view = Arc::new(ws.snapshot());
        let mut guard: RwLockWriteGuard<'_, Arc<ReaderView>> = self
            .reader
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = view;
    }

    /// 在写事务中执行 `f`:进入前快照写状态,`f` 返回 `Err` 时回滚到快照、
    /// 不发布;成功时发布读视图。
    ///
    /// 所有容器字段均为 `Arc`,`clone` 仅复制句柄,故快照/回滚廉价。此机制保证
    /// 任何失败的写操作对读者零可见、不留半写(FC-MEM-POST-002 泛化),并让
    /// 失败写入不残留命名空间登记等副作用。
    pub(crate) fn write_tx<T>(&self, f: impl FnOnce(&mut WriterState) -> Result<T>) -> Result<T> {
        let mut ws = self.write();
        let snapshot = ws.clone();
        match f(&mut ws) {
            Ok(value) => {
                // WAL 先于可见性写入:持久失败则整体回滚,绝不发布半持久状态。
                let ops = std::mem::take(&mut ws.pending);
                if let Some(persist) = &self.persist {
                    if let Err(error) = persist.log(&ops) {
                        // WAL 未落盘:整批回滚,写入对读者零可见(FC-MEM-POST-002)。
                        *ws = snapshot;
                        return Err(error);
                    }
                    // WAL 落盘即提交点:其后 flush 只回收已物化的 WAL 前缀(Checkpoint),失败不回滚,
                    // 否则内存回滚与重启后 WAL 重放会矛盾(「失败却持久」)。失败不丢:
                    // WAL 持续增长并由下次写重试,`stats().wal_bytes` 可观测(设计 04 §3.2)。
                    persist.maybe_flush(&mut ws, &self.config).ok();
                }
                self.publish(&ws);
                Ok(value)
            }
            Err(error) => {
                *ws = snapshot;
                Err(error)
            }
        }
    }

    /// 是否需要统计读路径命中(有持久层落 WAL 或开启自动遗忘)。
    ///
    /// 两者皆无时(默认纯内存库)不缓冲命中,避免无消费者时缓冲无限增长。
    pub(crate) fn tracks_access_hits(&self) -> bool {
        self.persist.is_some() || self.config.retention.is_some()
    }

    /// 记录一批读路径命中到访问缓冲(热路径仅一次内存追加,零写放大)。
    ///
    /// 缓冲条目数有上限(见 [`MAX_ACCESS_BUFFER_ENTRIES`]):达到上限后新的
    /// `RowId` 被丢弃(已有键继续累加)。维护线程停止/panic 时缓冲不会无界增长;
    /// 丢弃只影响遗忘速度估计,不影响可见性与检索正确性(FC-LIFE-POST-004)。
    pub(crate) fn record_hits(&self, rowids: impl IntoIterator<Item = RowId>) {
        let mut buffer = self
            .access_buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for rowid in rowids {
            if !buffer.contains_key(&rowid) && buffer.len() >= MAX_ACCESS_BUFFER_ENTRIES {
                continue;
            }
            let entry = buffer.entry(rowid).or_insert(0);
            *entry = entry.saturating_add(1);
        }
    }

    /// 把访问缓冲合并进写状态并落 WAL(后台维护按 `access_flush_interval` 调用)。
    ///
    /// 不可见记录(此时已删除/过期)的增量丢弃;WAL 失败不回滚内存统计——
    /// 访问计数只影响遗忘速度估计,丢失可接受(FC-LIFE-POST-004)。
    pub(crate) fn flush_access(&self, now_ms: i64) {
        let buffered: Vec<(RowId, u32)> = {
            let mut buffer = self
                .access_buffer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            buffer.drain().collect()
        };
        if buffered.is_empty() {
            return;
        }
        let result = self.write_tx(|ws| {
            for (rowid, delta) in &buffered {
                let visible = ws.latest.get(rowid).is_some_and(|slot| {
                    let data = &ws.slots[slot.get() as usize];
                    data.is_live(now_ms)
                });
                if !visible {
                    continue;
                }
                {
                    let stat = Arc::make_mut(&mut ws.access).entry(*rowid).or_default();
                    stat.access_count = stat.access_count.saturating_add(*delta);
                    stat.last_access_ms = now_ms;
                }
                ws.mark_access_dirty_by(*rowid, *delta);
                let seqno = ws.alloc_seqno()?;
                ws.pending.push(super::write_op::WriteOp::Access {
                    rowid: *rowid,
                    seqno,
                    at_ms: now_ms,
                    access_delta: *delta,
                    importance_delta: 0.0,
                });
            }
            Ok(())
        });
        // reason: 访问统计为尽力而为;WAL 失败时该批增量丢失,只影响遗忘速度
        // 估计,不影响记录可见性与检索正确性(FC-LIFE-POST-004)。
        let _ = result.ok();
    }

    /// 记录最近一次后台遗忘报告(I23 审计)。
    pub(crate) fn set_retain_report(&self, report: RetainReport) {
        *self
            .retain_report
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(report);
    }

    /// 最近一次后台遗忘报告(未开启自动遗忘时 `None`)。
    pub(crate) fn retain_report(&self) -> Option<RetainReport> {
        self.retain_report
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 记录一次查询延迟采样(毫秒)。
    pub(crate) fn record_query_latency(&self, latency_ms: f64) {
        self.latency
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .record(latency_ms);
    }

    /// 克隆查询延迟直方图。
    pub(crate) fn latency_histogram(&self) -> Histogram {
        self.latency
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}
