//! L2 存储基础设施:目录布局、原子写入、文件锁与协调句柄(`storage/`)。
//!
//! - 目录布局遵循设计 04 §1:`current` / `MANIFEST.<v>` / `wal/` / `segments/` / `trash/`;
//! - 所有元数据文件先写 `.tmp` 再 `rename` 提交,绝不原地覆盖(设计 04 §6);
//! - [`FileLock`] 提供单写者独占(设计 16 §3):基于 `std::fs::File::try_lock` 的
//!   OS 咨询锁,进程异常终止由内核自动释放,无需租约/接管;
//! - [`Store`] 协调 WAL 追加、增量段 flush 与恢复,是 L2 持久化的核心句柄。
//!
//! > `flush` 为**增量段**(L5 起):只把未落盘槽位与跨段 delta 写成新段,旧段保持活跃、
//! > MANIFEST 追加提交,段数由 compaction 控制;每条写入先追加 WAL(WAL-before-visible),
//! > 按 [`FsyncPolicy`] 决定持久确认时机。
//!
//! # 子模块
//!
//! * `backend` —— 存储后端抽象(`Storage`)与只读字节视图(`RawBytes`)。
//! * `layout` —— 目录布局常量、文件名与路径安全解析。
//! * `fs` —— 桌面/服务器文件系统后端 [`FsStorage`]。
//! * `mem` —— 纯内存后端 [`MemStorage`] (WASM/测试)。
//! * `io` —— 原子写入等文件系统自由函数与独占文件锁 [`FileLock`]。

mod backend;
mod fs;
mod io;
mod layout;
mod mem;

#[cfg(test)]
mod tests;

pub use backend::{FileMeta, RawBytes, Storage};
pub use fs::FsStorage;
pub use mem::MemStorage;

pub(crate) use io::{
    ensure_dir, exists, list_dir, read_file, read_file_opt, remove_if_exists, write_atomic,
};
// 旧路径 `crate::persist::storage::X` 要保持可达;crate 里只在测试或子模块内使用,放行警告。
#[allow(unused_imports)]
pub(crate) use io::{FileLock, read_prefix, rename, truncate, write_new};
pub(crate) use layout::{
    CURRENT_FILE, MANIFEST_KEEP, SEGMENTS_DIR, TRASH_DIR, WAL_DIR, hidx_name, manifest_name,
    msec_name, parse_manifest_name, resolve, vsec_name,
};
// 旧路径 `crate::persist::storage::X` 要保持可达;crate 里只在测试下经旧路径使用,放行警告。
#[allow(unused_imports)]
pub(crate) use layout::{LOCK_FILE, MANIFEST_PREFIX};
// 旧路径 `crate::persist::storage::MemLockToken` 要保持可达;库内不经旧路径使用,放行警告。
#[allow(unused_imports)]
pub(crate) use mem::MemLockToken;
