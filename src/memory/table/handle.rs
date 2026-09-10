//! 内存表句柄:写锁 / 读锁 / 写事务(`table/handle.rs`)。

use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::core::error::Result;
use crate::memory::config::Config;

use super::PersistHook;
use super::state::WriterState;
use super::view::ReaderView;

/// 内存表:写状态 + 已发布读视图 + 配置 + 可选持久钩子。
pub(crate) struct Table {
    pub(crate) writer: Mutex<WriterState>,
    pub(crate) reader: RwLock<Arc<ReaderView>>,
    pub(crate) config: Arc<Config>,
    /// 持久层写日志钩子;`None` = 纯内存(L1)。
    pub(crate) persist: Option<Arc<dyn PersistHook>>,
}

impl Table {
    /// 以给定配置新建空表(无持久钩子)。
    pub(crate) fn new(config: Arc<Config>) -> Self {
        Self::new_with(config, None)
    }

    /// 以给定配置与持久钩子新建空表。
    pub(crate) fn new_with(config: Arc<Config>, persist: Option<Arc<dyn PersistHook>>) -> Self {
        let writer = WriterState::new();
        let view = Arc::new(writer.snapshot());
        Self {
            writer: Mutex::new(writer),
            reader: RwLock::new(view),
            config,
            persist,
        }
    }

    /// 以恢复出的写状态构造表(持久化打开;状态已含段与 WAL 重放结果)。
    pub(crate) fn from_state(
        config: Arc<Config>,
        state: WriterState,
        persist: Option<Arc<dyn PersistHook>>,
    ) -> Self {
        let view = Arc::new(state.snapshot());
        Self {
            writer: Mutex::new(state),
            reader: RwLock::new(view),
            config,
            persist,
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
                        *ws = snapshot;
                        return Err(error);
                    }
                    // WAL 容量等阈值兜底:越界即全量快照 flush(设计 04 §3.2)。
                    if let Err(error) = persist.maybe_flush(&ws, &self.config) {
                        *ws = snapshot;
                        return Err(error);
                    }
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
}
