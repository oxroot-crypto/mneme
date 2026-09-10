//! 崩溃注入与 I/O 观测钩子(`hook.rs`,设计 04 §10.1)。
//!
//! 供测试在每次 `write`/`fsync`/`rename` 前注入故障(丢写/翻转字节/截断/报错),
//! 以验证"崩溃后状态 = 已确认操作前缀"(设计 14 §2)。生产不设置即无开销。

/// 待注入的 I/O 动作。
#[derive(Debug, Clone, Copy)]
pub enum IoAction<'a> {
    /// 写入动作:文件、偏移与长度。
    Write {
        /// 相对库根的文件名。
        file: &'a str,
        /// 写入偏移(字节)。
        offset: u64,
        /// 写入长度(字节)。
        len: usize,
    },
    /// fsync 动作。
    Fsync {
        /// 相对库根的文件名。
        file: &'a str,
    },
    /// 重命名动作(原子提交点)。
    Rename {
        /// 源相对路径。
        from: &'a str,
        /// 目标相对路径。
        to: &'a str,
    },
}

/// I/O 前置钩子:在每次 write/fsync/rename 前调用,可返回错误注入故障。
pub trait FsyncHook: Send + Sync {
    /// 在动作发生前调用;返回 `Err` 表示注入故障,调用方应中止该动作。
    ///
    /// # Errors
    /// 返回 [`std::io::Error`] 即模拟该 I/O 失败。
    fn before(&self, action: IoAction<'_>) -> std::io::Result<()>;
}
