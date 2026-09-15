//! 纯内存后端(`MemStorage`;WASM/测试)。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::core::error::{MnemeError, Result};

use super::backend::{FileMeta, Storage};
use super::layout::resolve;

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
