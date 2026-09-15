//! 原子写入等文件系统自由函数与独占文件锁(`FileLock`)。

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::core::error::{MnemeError, Result};

use super::layout::{LOCK_FILE, resolve};

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

/// 读取文件前缀(至多 `max` 字节;文件更短时返回全部)。
///
/// 用于段打开时的 4 字节信封探测,避免为非加密大段白付一遍整读
/// (`FC-PERSIST-INV-021`/`FC-PERSIST-CPLX-007`)。
///
/// # Errors
/// 文件不存在或 I/O 失败时返回 [`MnemeError::Io`]。
pub(crate) fn read_prefix(root: &Path, rel: &str, max: usize) -> Result<Vec<u8>> {
    let target = resolve(root, rel)?;
    let mut file = File::open(target)?;
    let mut bytes = vec![0_u8; max];
    let mut filled = 0;
    while filled < bytes.len() {
        let read = file.read(&mut bytes[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    bytes.truncate(filled);
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
pub(super) fn tmp_path(path: &Path) -> PathBuf {
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
/// (设计 16 §3;`File::try_lock` 自 Rust 1.89 起稳定,本库 MSRV 1.93 满足;
/// Windows 用 `LockFileEx`)。
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
