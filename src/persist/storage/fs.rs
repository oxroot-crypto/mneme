//! 桌面/服务器文件系统后端(`FsStorage`)。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(all(feature = "mmap", not(feature = "wasm")))]
use std::fs::File;

use crate::core::error::Result;

#[cfg(all(feature = "mmap", not(feature = "wasm")))]
use super::backend::RawBytes;
use super::backend::{FileMeta, Storage};
use super::io::{
    FileLock, ensure_dir, exists, list_dir, read_file, read_file_opt, read_prefix,
    remove_if_exists, rename, truncate, write_atomic, write_new,
};
use super::layout::resolve;

/// 桌面/服务器文件系统后端(std 自由函数实现,见 `io` 子模块)。
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
        // 段文件 write-once,映射期不截断/改写(见 src/persist/source/backend.rs 的 SAFETY 证明)。
        Ok(RawBytes::Mmap(crate::persist::source::map_readonly(&file)?))
    }

    fn root_exists(&self) -> Result<bool> {
        Ok(self.root.try_exists()?)
    }

    fn try_lock(&self) -> Result<Box<dyn std::any::Any + Send + Sync>> {
        Ok(Box::new(FileLock::acquire(&self.root)?))
    }
}
