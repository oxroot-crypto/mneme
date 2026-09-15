//! async 门面个类型 `AsyncNamespace` 同阻塞桥 `run_blocking`。

use crate::memory::namespace::Namespace;

/// 把阻塞调用移入 tokio 阻塞线程池。
///
/// # Panics
///
/// 阻塞池任务异常终止(取消/panic)时 panic;按设计 16 §4 的文档化例外传播——
/// 核心库承诺不 panic,出现即属运行时故障,绝不静默吞掉。
pub(super) async fn run_blocking<T, F>(task: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        // reason: 核心库不 panic(FC-GLOBAL-ERR-001);阻塞任务异常终止属运行时故障,
        // 按设计 16 §4 的文档化 panic 例外传播(同 `filter!` 宏的文档化例外)。
        .expect("async 门面:阻塞任务异常终止")
}

/// [`Namespace`] 的异步门面(feature `async`)。
///
/// 轻量包装类型:与同步 `Namespace` 共享同一底层句柄与写锁,所有方法等价于
/// `spawn_blocking` 包装的同步调用(I14)。经 [`Namespace::into_async`] 构造;
/// 本类型 `Send + Sync`。
#[derive(Debug, Clone)]
pub struct AsyncNamespace {
    /// 被包装的同步命名空间句柄。
    pub(super) inner: Namespace,
}

impl Namespace {
    /// 转为异步门面(共享同一句柄与写锁)。
    ///
    /// # Returns
    ///
    /// 包装本句柄的 [`AsyncNamespace`]。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mneme::{Mneme, Record};
    ///
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo").into_async();
    /// let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    /// runtime.block_on(async {
    ///     ns.insert(Record::new(vec![1.0, 0.0])).await.unwrap();
    /// });
    /// ```
    pub fn into_async(self) -> AsyncNamespace {
        AsyncNamespace { inner: self }
    }
}
