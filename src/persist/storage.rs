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

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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

/// 文件元数据(不依赖 `std::fs`,宿主后端同样可实现;设计 12 §3.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    /// 文件字节数。
    pub len: u64,
}

/// 存储后端抽象(根目录绑定;路径均为库内相对路径,见设计 12 §3.1)。
///
/// 桌面/服务器默认 [`FsStorage`];WASM/宿主可注入 [`MemStorage`] 或自定义后端
/// (`Builder::storage`)。文件锁在 trait 内提供等价能力,锁文件/目录语义由实现
/// 负责;写路径的原子提交(`write_atomic` = 临时文件 + rename + fsync)是后端契约。
pub trait Storage: Send + Sync + std::fmt::Debug {
    /// 读取整个文件。
    ///
    /// # Errors
    /// 文件不存在或 I/O 失败时返回结构化错误。
    fn read_file(&self, rel: &str) -> Result<Vec<u8>>;

    /// 读取整个文件;不存在返回 `None`。
    ///
    /// # Errors
    /// 其他 I/O 失败返回结构化错误。
    fn read_file_opt(&self, rel: &str) -> Result<Option<Vec<u8>>>;

    /// 读取文件前缀(至多 `max` 字节;文件更短时返回全部)。
    ///
    /// 段打开仅需前 4 字节做信封探测,不得为此整读大段(FC-PERSIST-INV-021);
    /// 默认实现经 [`Storage::read_file`] 整读后截断(内存后端语义正确),
    /// 文件后端应覆写为真正的部分读取。
    ///
    /// # Errors
    /// 文件不存在或 I/O 失败时返回结构化错误。
    fn read_prefix(&self, rel: &str, max: usize) -> Result<Vec<u8>> {
        let mut bytes = self.read_file(rel)?;
        bytes.truncate(max);
        Ok(bytes)
    }

    /// 原子写入:临时文件 → fsync → rename → 目录 fsync(绝不原地覆盖)。
    ///
    /// # Errors
    /// 任一步 I/O 失败时返回结构化错误。
    fn write_atomic(&self, rel: &str, bytes: &[u8]) -> Result<()>;

    /// 只创建不覆盖地写入(目标已存在则失败)。
    ///
    /// # Errors
    /// 目标已存在或 I/O 失败时返回结构化错误。
    fn write_new(&self, rel: &str, bytes: &[u8]) -> Result<()>;

    /// 追加写入并返回追加后的文件长度。
    ///
    /// # Errors
    /// I/O 失败时返回结构化错误。
    fn append(&self, rel: &str, bytes: &[u8]) -> Result<u64>;

    /// 把文件截断到 `len` 字节并尽力 fsync。
    ///
    /// # Errors
    /// 文件不存在或 I/O 失败时返回结构化错误。
    fn truncate(&self, rel: &str, len: u64) -> Result<()>;

    /// 把文件 fsync 到稳定存储。
    ///
    /// # Errors
    /// I/O 失败时返回结构化错误。
    fn sync(&self, rel: &str) -> Result<()>;

    /// 列出目录下的文件名(不含子目录),目录不存在返回空。
    ///
    /// # Errors
    /// I/O 失败时返回结构化错误。
    fn list_dir(&self, rel: &str) -> Result<Vec<String>>;

    /// 创建目录(幂等)。
    ///
    /// # Errors
    /// I/O 失败时返回结构化错误。
    fn ensure_dir(&self, rel: &str) -> Result<()>;

    /// 删除文件;不存在视为成功。
    ///
    /// # Errors
    /// 其他 I/O 失败返回结构化错误。
    fn remove_if_exists(&self, rel: &str) -> Result<()>;

    /// 重命名文件(同后端内)。
    ///
    /// # Errors
    /// I/O 失败时返回结构化错误。
    fn rename(&self, from: &str, to: &str) -> Result<()>;

    /// 文件是否存在。
    ///
    /// # Errors
    /// 查询失败时返回结构化错误。
    fn exists(&self, rel: &str) -> Result<bool>;

    /// 文件元数据。
    ///
    /// # Errors
    /// 文件不存在或查询失败时返回结构化错误。
    fn stat(&self, rel: &str) -> Result<FileMeta>;

    /// 以只读视图打开文件:支持零拷贝的后端可返回映射,否则返回整文件自有缓冲。
    /// 默认实现 = [`Storage::read_file`](实现者不必关心 mmap)。
    ///
    /// # 前置条件(返回 `RawBytes::Mmap` 的后端)
    /// 视图存活期内文件不得被原地改写或截断;库内仅对 **write-once 段文件**
    /// 调用本方法(段经临时文件 + rename 生成,compaction 只重命名/删除目录项)。
    /// 对 `wal` / `current` / `MANIFEST.*` 等会被追加或截断的文件返回映射,
    /// 存在 `SIGBUS` 风险,自定义后端必须改为返回 `RawBytes::Owned`。
    ///
    /// # Errors
    /// 读取失败时返回结构化错误。
    fn open_bytes(&self, rel: &str) -> Result<RawBytes> {
        Ok(RawBytes::Owned(self.read_file(rel)?.into_boxed_slice()))
    }

    /// 库根目录是否存在(只读打开的前置检查;内存后端恒 `true`)。
    ///
    /// # Errors
    /// 查询失败时返回结构化错误。
    fn root_exists(&self) -> Result<bool> {
        Ok(true)
    }

    /// 尝试获取锁文件(`LOCK`)的独占锁。
    ///
    /// # Returns
    /// 成功时返回不透明守卫;`Drop` 即释放。锁被活实例持有 → [`MnemeError::Busy`]。
    ///
    /// # Errors
    /// 见上;I/O 失败返回 [`MnemeError::Io`]。
    fn try_lock(&self) -> Result<Box<dyn std::any::Any + Send + Sync>>;
}

/// [`Storage::open_bytes`] 的只读字节视图。
#[derive(Debug)]
pub enum RawBytes {
    /// 内核按页惰性载入的只读映射(feature `mmap`)。
    #[cfg(all(feature = "mmap", not(feature = "wasm")))]
    Mmap(memmap2::Mmap),
    /// 整文件自有缓冲。
    Owned(Box<[u8]>),
}

impl RawBytes {
    /// 只读字节切片。
    ///
    /// # Returns
    /// 覆盖整个文件的连续只读字节;`mmap` 后端下由内核按页惰性载入。
    pub fn as_slice(&self) -> &[u8] {
        match self {
            #[cfg(all(feature = "mmap", not(feature = "wasm")))]
            Self::Mmap(map) => map,
            Self::Owned(bytes) => bytes,
        }
    }

    /// 字节数。
    ///
    /// # Returns
    /// 文件长度(字节)。
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// 是否为零字节。
    ///
    /// # Returns
    /// 长度为零时返回 `true`。
    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
}

/// 桌面/服务器文件系统后端(std 自由函数实现,见本模块顶部)。
#[derive(Debug, Clone)]
pub struct FsStorage {
    root: PathBuf,
}

impl FsStorage {
    /// 以库根目录建立后端。
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }
}

impl Storage for FsStorage {
    fn read_file(&self, rel: &str) -> Result<Vec<u8>> {
        read_file(&self.root, rel)
    }

    fn read_file_opt(&self, rel: &str) -> Result<Option<Vec<u8>>> {
        read_file_opt(&self.root, rel)
    }

    fn read_prefix(&self, rel: &str, max: usize) -> Result<Vec<u8>> {
        read_prefix(&self.root, rel, max)
    }

    fn write_atomic(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        write_atomic(&self.root, rel, bytes)
    }

    fn write_new(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        write_new(&self.root, rel, bytes)
    }

    fn append(&self, rel: &str, bytes: &[u8]) -> Result<u64> {
        let target = resolve(&self.root, rel)?;
        let mut file = OpenOptions::new().append(true).open(&target)?;
        file.write_all(bytes)?;
        Ok(file.metadata()?.len())
    }

    fn truncate(&self, rel: &str, len: u64) -> Result<()> {
        truncate(&self.root, rel, len)
    }

    fn sync(&self, rel: &str) -> Result<()> {
        let target = resolve(&self.root, rel)?;
        let file = OpenOptions::new().read(true).open(target)?;
        file.sync_all()?;
        Ok(())
    }

    fn list_dir(&self, rel: &str) -> Result<Vec<String>> {
        list_dir(&self.root, rel)
    }

    fn ensure_dir(&self, rel: &str) -> Result<()> {
        ensure_dir(&self.root.join(rel))
    }

    fn remove_if_exists(&self, rel: &str) -> Result<()> {
        remove_if_exists(&self.root.join(rel))
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        let from = resolve(&self.root, from)?;
        let to = resolve(&self.root, to)?;
        rename(&from, &to)
    }

    fn exists(&self, rel: &str) -> Result<bool> {
        exists(&self.root.join(rel))
    }

    fn stat(&self, rel: &str) -> Result<FileMeta> {
        let meta = fs::metadata(self.root.join(rel))?;
        Ok(FileMeta { len: meta.len() })
    }

    #[cfg(all(feature = "mmap", not(feature = "wasm")))]
    fn open_bytes(&self, rel: &str) -> Result<RawBytes> {
        let path = self.root.join(rel);
        let file = File::open(path)?;
        // 只读映射的安全前置(SAFETY 证明)集中在 `source::map_readonly`:
        // 段文件 write-once,映射期不截断/改写(见 src/persist/source.rs)。
        Ok(RawBytes::Mmap(crate::persist::source::map_readonly(&file)?))
    }

    fn root_exists(&self) -> Result<bool> {
        Ok(self.root.try_exists()?)
    }

    fn try_lock(&self) -> Result<Box<dyn std::any::Any + Send + Sync>> {
        Ok(Box::new(FileLock::acquire(&self.root)?))
    }
}

/// 纯内存后端(WASM/测试;无 mmap、无 OS 锁)。
///
/// 布局在内存中以相对路径为键;`try_lock` 用进程内互斥模拟(单进程多实例互斥,
/// 不跨进程——WASM/浏览器语义)。
#[derive(Debug, Default)]
pub struct MemStorage {
    files: Mutex<HashMap<String, Vec<u8>>>,
    dirs: Mutex<HashSet<String>>,
    /// 进程内独占标志(模拟 OS 咨询锁;进程内多实例互斥)。
    locked: Arc<AtomicBool>,
}

impl MemStorage {
    /// 新建空后端。
    pub fn new() -> Self {
        Self::default()
    }

    /// 校验相对路径(拒绝绝对路径与 `..`)。
    fn check(rel: &str) -> Result<()> {
        resolve(Path::new("."), rel).map(|_path| ())
    }
}

impl Storage for MemStorage {
    fn read_file(&self, rel: &str) -> Result<Vec<u8>> {
        Self::check(rel)?;
        self.files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(rel)
            .cloned()
            .ok_or_else(|| {
                MnemeError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    rel.to_string(),
                ))
            })
    }

    fn read_file_opt(&self, rel: &str) -> Result<Option<Vec<u8>>> {
        match self.read_file(rel) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(MnemeError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn read_prefix(&self, rel: &str, max: usize) -> Result<Vec<u8>> {
        Self::check(rel)?;
        self.files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(rel)
            .map(|bytes| bytes[..bytes.len().min(max)].to_vec())
            .ok_or_else(|| {
                MnemeError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    rel.to_string(),
                ))
            })
    }

    fn write_atomic(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        self.write_new_or_replace(rel, bytes)
    }

    fn write_new(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        Self::check(rel)?;
        let mut files = self
            .files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if files.contains_key(rel) {
            return Err(MnemeError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                rel.to_string(),
            )));
        }
        files.insert(rel.to_string(), bytes.to_vec());
        Ok(())
    }

    fn append(&self, rel: &str, bytes: &[u8]) -> Result<u64> {
        Self::check(rel)?;
        let mut files = self
            .files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = files.entry(rel.to_string()).or_default();
        entry.extend_from_slice(bytes);
        Ok(entry.len() as u64)
    }

    fn truncate(&self, rel: &str, len: u64) -> Result<()> {
        Self::check(rel)?;
        let mut files = self
            .files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = files.get_mut(rel).ok_or_else(|| {
            MnemeError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                rel.to_string(),
            ))
        })?;
        entry.truncate(len as usize);
        Ok(())
    }

    fn sync(&self, _rel: &str) -> Result<()> {
        Ok(())
    }

    fn list_dir(&self, rel: &str) -> Result<Vec<String>> {
        Self::check(rel)?;
        let prefix = if rel.is_empty() {
            String::new()
        } else {
            format!("{rel}/")
        };
        let files = self
            .files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut names: Vec<String> = files
            .keys()
            .filter_map(|key| {
                key.strip_prefix(&prefix).and_then(|rest| {
                    (!rest.is_empty() && !rest.contains('/')).then(|| rest.to_string())
                })
            })
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    fn ensure_dir(&self, rel: &str) -> Result<()> {
        Self::check(rel)?;
        self.dirs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(rel.to_string());
        Ok(())
    }

    fn remove_if_exists(&self, rel: &str) -> Result<()> {
        Self::check(rel)?;
        self.files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(rel);
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        Self::check(from)?;
        Self::check(to)?;
        let mut files = self
            .files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(bytes) = files.remove(from) else {
            return Err(MnemeError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                from.to_string(),
            )));
        };
        files.insert(to.to_string(), bytes);
        Ok(())
    }

    fn exists(&self, rel: &str) -> Result<bool> {
        Self::check(rel)?;
        Ok(self
            .files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(rel))
    }

    fn stat(&self, rel: &str) -> Result<FileMeta> {
        let bytes = self.read_file(rel)?;
        Ok(FileMeta {
            len: bytes.len() as u64,
        })
    }

    fn try_lock(&self) -> Result<Box<dyn std::any::Any + Send + Sync>> {
        // 进程内互斥:同一进程第二个实例立即 `Busy`;守卫 Drop 即释放。
        if self.locked.swap(true, Ordering::AcqRel) {
            return Err(MnemeError::Busy("库已被另一实例打开(内存后端)"));
        }
        Ok(Box::new(MemLockToken(Arc::clone(&self.locked))))
    }
}

impl MemStorage {
    /// 替换写入(原子语义在内存下即直接替换)。
    fn write_new_or_replace(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        Self::check(rel)?;
        self.files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(rel.to_string(), bytes.to_vec());
        Ok(())
    }
}

/// 内存后端的锁守卫:`Drop` 释放进程内独占标志。
#[derive(Debug)]
pub(crate) struct MemLockToken(Arc<AtomicBool>);

impl Drop for MemLockToken {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

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
