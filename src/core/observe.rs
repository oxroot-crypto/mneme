//! 可观测性钩子(L12,设计 12 §4)。
//!
//! 事件级可观测(而非仅 `stats()` 轮询),但默认零成本:未注册 `Observer` 时
//! 引擎不构造事件、不做任何分支外工作;回调 panic 被隔离(`catch_unwind`),
//! 绝不改变引擎行为(I30)。

use std::sync::Arc;
use std::time::Duration;

use crate::core::types::SegmentId;

/// 事件回调(宿主实现;可桥接到 `tracing`/metrics/OpenTelemetry)。
pub trait Observer: Send + Sync {
    /// 收到一个引擎事件;默认空实现。
    fn on_event(&self, event: Event);
}

/// 引擎事件。
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// 一次检索完成。
    Query {
        /// 耗时。
        took: Duration,
        /// 候选数(过滤后)。
        candidates: usize,
        /// 返回条数。
        returned: usize,
        /// 通道数(1 = 单通道,2 = 向量 + 文本)。
        channels: u8,
    },
    /// 一次写事务提交(`bytes` = 已编码的 WAL 负载字节;内存库为 0)。
    Write {
        /// 逻辑写操作。
        op: WriteOp,
        /// 耗时。
        took: Duration,
        /// 负载字节。
        bytes: usize,
    },
    /// 一次段 flush 提交。
    Flush {
        /// 新增段数。
        segments: usize,
        /// 当前 WAL 字节数。
        wal_bytes: u64,
    },
    /// 一次 compaction 提交。
    Compaction {
        /// 被合并的段。
        segments: Vec<SegmentId>,
        /// 耗时。
        took: Duration,
        /// 新段行数。
        rows_out: u64,
    },
    /// 一次操作失败(粗分类;精确定位仍以 `MnemeError` 为准)。
    Error {
        /// 错误粗分类。
        kind: ErrorKind,
        /// 出错位置标签(稳定字符串,便于聚合)。
        context: &'static str,
    },
}

/// 逻辑写操作(粗粒度,面向观测聚合)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOp {
    /// 单条插入。
    Insert,
    /// 批量插入。
    InsertBatch,
    /// 更新。
    Update,
    /// 删除。
    Delete,
    /// 访问统计更新。
    Touch,
    /// 建立关系。
    Relate,
    /// 删除关系。
    Unrelate,
    /// 遗忘。
    Forget,
    /// 显式保留。
    Retain,
    /// 信念修订。
    Supersede,
    /// 记忆沉淀。
    Consolidate,
    /// 命名空间注销。
    DropNamespace,
}

/// `MnemeError` 的粗分类(设计 12 §4;精确定位仍以 `MnemeError` 为准)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 底层 I/O 错误。
    Io,
    /// 数据损坏。
    Corrupted,
    /// 库被占用。
    Busy,
    /// 负载超限。
    TooLarge,
    /// 超出配额。
    LimitExceeded,
    /// 格式版本不支持。
    UnsupportedVersion,
    /// 库已关闭。
    Closed,
    /// 配置非法。
    Config,
    /// 能力未落地/不适用。
    Unsupported,
    /// 其它(维度/度量/key/解析等)。
    Other,
}

impl ErrorKind {
    /// 由统一错误映射为粗分类。
    pub(crate) fn of(error: &crate::core::error::MnemeError) -> Self {
        use crate::core::error::MnemeError;
        match error {
            MnemeError::Io(_) => Self::Io,
            MnemeError::Corrupted { .. } => Self::Corrupted,
            MnemeError::Busy(_) => Self::Busy,
            MnemeError::TooLarge { .. } => Self::TooLarge,
            MnemeError::LimitExceeded { .. } => Self::LimitExceeded,
            MnemeError::UnsupportedVersion { .. } => Self::UnsupportedVersion,
            MnemeError::Closed => Self::Closed,
            MnemeError::Config { .. } => Self::Config,
            MnemeError::Unsupported { .. } => Self::Unsupported,
            _ => Self::Other,
        }
    }
}

/// 安全派发事件:回调 panic 被隔离,绝不改变引擎行为(I30)。
///
/// 未注册(`None`)时直接返回,零成本。
pub(crate) fn emit(observer: Option<&Arc<dyn Observer>>, event: Event) {
    let Some(observer) = observer else {
        return;
    };
    // reason: 隔离宿主回调 panic,观察者不得影响引擎(FC-DEPLOY-INV-030);
    // 事件为尽力而为,不重试、不传播。
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        observer.on_event(event);
    }))
    .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counter(std::sync::atomic::AtomicUsize);

    impl Observer for Counter {
        fn on_event(&self, _event: Event) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    struct Panicker;

    impl Observer for Panicker {
        fn on_event(&self, _event: Event) {
            panic!("boom");
        }
    }

    /// FC-DEPLOY-INV-030:事件派发;panic 回调被隔离且不中断调用方。
    #[test]
    fn emit_delivers_and_isolates_panics() {
        let counter = Arc::new(Counter(std::sync::atomic::AtomicUsize::new(0)));
        let observer: Arc<dyn Observer> = Arc::clone(&counter) as Arc<dyn Observer>;
        emit(
            Some(&observer),
            Event::Query {
                took: Duration::from_millis(1),
                candidates: 3,
                returned: 2,
                channels: 1,
            },
        );
        assert_eq!(counter.0.load(std::sync::atomic::Ordering::Relaxed), 1);
        let panicker: Arc<dyn Observer> = Arc::new(Panicker);
        // 不 panic、不返回值:调用方继续执行。
        emit(
            Some(&panicker),
            Event::Error {
                kind: ErrorKind::Other,
                context: "test",
            },
        );
    }
}
