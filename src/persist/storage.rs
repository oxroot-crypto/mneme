//! L2 存储基础设施:目录布局、原子写入、文件锁与协调句柄(`storage.rs`)。
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

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::core::error::{MnemeError, Result};

/// `current` 指针文件名。
pub(crate) const CURRENT_FILE: &str = "current";
/// MANIFEST 文件名前缀。
pub(crate) const MANIFEST_PREFIX: &str = "MANIFEST.";
/// WAL 子目录。
pub(crate) const WAL_DIR: &str = "wal";
/// 段子目录。
pub(crate) const SEGMENTS_DIR: &str = "segments";
/// 回收站子目录。
pub(crate) const TRASH_DIR: &str = "trash";
/// 独占锁文件名。
pub(crate) const LOCK_FILE: &str = "LOCK";

/// 保留的 MANIFEST 版本数(设计 04 §6:任意一步崩溃至少留一个完整可用版本)。
pub(crate) const MANIFEST_KEEP: usize = 2;

/// MANIFEST 版本文件名(如 `MANIFEST.000042`)。
pub(crate) fn manifest_name(version: u64) -> String {
    format!("{MANIFEST_PREFIX}{version:06}")
}

/// 从 `MANIFEST.<v>` 文件名解析版本号;不匹配返回 `None`。
pub(crate) fn parse_manifest_name(name: &str) -> Option<u64> {
    name.strip_prefix(MANIFEST_PREFIX)?.parse().ok()
}

/// 段向量文件名。
pub(crate) fn vsec_name(segment_id: u32) -> String {
    format!("seg_{segment_id:06}.vsec")
}

/// 段元数据文件名。
pub(crate) fn msec_name(segment_id: u32) -> String {
    format!("seg_{segment_id:06}.msec")
}

/// 段 HNSW 图文件名(L3 起)。
pub(crate) fn hidx_name(segment_id: u32) -> String {
    format!("seg_{segment_id:06}.hidx")
}

/// 把相对路径拼到根目录下,拒绝绝对路径与 `..` 目录穿越。
///
/// # Errors
/// `rel` 为绝对路径或含 `..` 时返回 [`MnemeError::Config`]。
pub(crate) fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
    let path = Path::new(rel);
    if path.is_absolute()
        || path.has_root()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(MnemeError::Config {
            reason: "存储路径不得为绝对路径或包含 ..",
        });
    }
    Ok(root.join(path))
}

/// 原子写入:写 `rel.tmp` → fsync → rename 覆盖 `rel` → fsync 目录。
///
/// # Errors
/// 任一步 I/O 失败时返回 [`MnemeError::Io`]。
pub(crate) fn write_atomic(root: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let target = resolve(root, rel)?;
    let tmp = tmp_path(&target);
    {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, &target)?;
    sync_parent(&target)?;
    Ok(())
}

/// 只创建不覆盖地写入 `rel`(目标已存在则失败)。
///
/// # Errors
/// 目标已存在或 I/O 失败时返回 [`MnemeError::Io`]。
#[cfg(test)]
pub(crate) fn write_new(root: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let target = resolve(root, rel)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// 把文件截断到 `len` 字节并 fsync(用于丢弃 WAL 撕裂尾部)。
///
/// # Errors
/// 文件不存在或 I/O 失败时返回 [`MnemeError::Io`]。
pub(crate) fn truncate(root: &Path, rel: &str, len: u64) -> Result<()> {
    let target = resolve(root, rel)?;
    let file = OpenOptions::new().write(true).open(&target)?;
    file.set_len(len)?;
    file.sync_all()?;
    Ok(())
}

/// 读取整个文件。
///
/// # Errors
/// 文件不存在或 I/O 失败时返回 [`MnemeError::Io`]。
pub(crate) fn read_file(root: &Path, rel: &str) -> Result<Vec<u8>> {
    let target = resolve(root, rel)?;
    let mut file = File::open(target)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// 读取整个文件;文件不存在返回 `None`。
///
/// # Errors
/// 其他 I/O 失败返回 [`MnemeError::Io`]。
pub(crate) fn read_file_opt(root: &Path, rel: &str) -> Result<Option<Vec<u8>>> {
    match read_file(root, rel) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(MnemeError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// 列出目录下的条目名(不含子目录),目录不存在时返回空。
///
/// # Errors
/// I/O 失败时返回 [`MnemeError::Io`]。
pub(crate) fn list_dir(root: &Path, rel: &str) -> Result<Vec<String>> {
    let dir = resolve(root, rel)?;
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// 创建目录(幂等)。
///
/// # Errors
/// I/O 失败时返回 [`MnemeError::Io`]。
pub(crate) fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

/// 删除文件;不存在视为成功。
///
/// # Errors
/// 其他 I/O 失败返回 [`MnemeError::Io`]。
pub(crate) fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// 重命名文件(同目录/跨目录均可)。
///
/// # Errors
/// I/O 失败时返回 [`MnemeError::Io`]。
pub(crate) fn rename(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)?;
    sync_parent(to)?;
    Ok(())
}

/// 文件是否存在。
///
/// # Errors
/// 查询失败时返回 [`MnemeError::Io`]。
pub(crate) fn exists(path: &Path) -> Result<bool> {
    Ok(path.try_exists()?)
}

/// 返回 `<path>.tmp` 临时路径。
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(std::ffi::OsString::new, std::ffi::OsStr::to_os_string);
    name.push(".tmp");
    path.with_file_name(name)
}

/// fsync 文件所在目录(确保 rename 持久化)。目录不可打开时忽略(如 Windows 下目录句柄语义)。
fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && let Ok(file) = File::open(parent)
    {
        // reason: 目录 fsync 在部分平台不受支持;失败仅意味着目录项可能晚于数据落盘,
        // 由 MANIFEST write-once + 扫描兜底(设计 04 §6),故显式忽略该错误。
        file.sync_all().ok();
    }
    Ok(())
}

/// 独占文件锁:基于 `std::fs::File::try_lock` 的 OS 咨询锁。
///
/// 锁文件 `LOCK` 常驻不删除——删除会使不同 inode 各自可加锁,反而破坏互斥。
/// 互斥语义由内核持有的咨询锁提供:活实例持有时其它实例 `try_lock` 返回
/// [`std::fs::TryLockError::WouldBlock`](→ [`MnemeError::Busy`]);进程崩溃/退出时
/// 内核自动释放,后续实例可直接获取。无需租约刷新、心跳线程或陈旧接管
/// (设计 16 §3;`File::try_lock` 自 MSRV 1.93 起稳定,Windows 用 `LockFileEx`)。
pub(crate) struct FileLock {
    /// 持有 OS 咨询锁的文件句柄;`Drop` 关闭句柄即释放锁。
    _file: File,
}

impl FileLock {
    /// 在库根目录获取独占锁。
    ///
    /// # Errors
    /// 锁被其他活实例持有时返回 [`MnemeError::Busy`];I/O 失败返回 [`MnemeError::Io`]。
    pub(crate) fn acquire(root: &Path) -> Result<Self> {
        let path = root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                Err(MnemeError::Busy("库目录已被另一实例打开"))
            }
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // 显式解锁(等价于关闭句柄);锁文件保留,避免 inode 分裂破坏互斥。
        // reason: `Drop` 无法传播错误;句柄随本结构体关闭也会由内核释放锁。
        self._file.unlock().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 原子写入后可读回,`.tmp` 不残留。
    #[test]
    fn write_atomic_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_atomic(dir.path(), "current", b"42").expect("write");
        assert_eq!(read_file(dir.path(), "current").expect("read"), b"42");
        assert!(!exists(&tmp_path(&dir.path().join("current"))).expect("exists"));
    }

    /// `write_new` 不覆盖既有文件。
    #[test]
    fn write_new_does_not_overwrite() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_new(dir.path(), "a", b"1").expect("first");
        assert!(matches!(
            write_new(dir.path(), "a", b"2"),
            Err(crate::core::error::MnemeError::Io(_))
        ));
        assert_eq!(std::fs::read(dir.path().join("a")).expect("read"), b"1");
    }

    /// 路径穿越被拒绝。
    #[test]
    fn resolve_rejects_traversal() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(resolve(dir.path(), "../evil").is_err());
        assert!(resolve(dir.path(), "/abs").is_err());
        assert!(resolve(dir.path(), "wal/wal_000001.log").is_ok());
    }

    /// 锁互斥与释放后重取。
    #[test]
    fn file_lock_blocks_second_holder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = FileLock::acquire(dir.path()).expect("first");
        assert!(matches!(
            FileLock::acquire(dir.path()),
            Err(MnemeError::Busy(_))
        ));
        drop(first);
        assert!(FileLock::acquire(dir.path()).is_ok());
    }

    /// 锁文件已存在但无 OS 持有者(如崩溃残留)时不阻塞——OS 咨询锁已随进程释放。
    #[test]
    fn file_lock_acquires_when_lock_file_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(LOCK_FILE), b"999\n0\n").expect("write stale lock");
        assert!(FileLock::acquire(dir.path()).is_ok());
    }

    /// `Drop` 释放锁但不删除锁文件,避免不同 inode 各自加锁破坏互斥。
    #[test]
    fn file_lock_file_persists_after_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = FileLock::acquire(dir.path()).expect("acquire");
        let path = dir.path().join(LOCK_FILE);
        assert!(path.exists());
        drop(lock);
        assert!(path.exists(), "锁文件应保留");
        assert!(FileLock::acquire(dir.path()).is_ok(), "释放后可重新获取");
    }

    /// MANIFEST 文件名解析。
    #[test]
    fn manifest_name_roundtrip() {
        assert_eq!(manifest_name(42), "MANIFEST.000042");
        assert_eq!(parse_manifest_name("MANIFEST.000042"), Some(42));
        assert_eq!(parse_manifest_name("current"), None);
    }
}
