//! `trash` 延迟删除(设计 04 §9)。
//!
//! Windows 不允许删除被 mmap/句柄打开的文件:提交新 MANIFEST 后先把旧段文件
//! `rename` 进 `trash/`(rename 对打开中的文件是允许的),待最后一个读者释放后
//! 或下次启动时物理删除。本层读者不长期持有段句柄,故 `open`/`close` 时统一清理。

use std::path::Path;

use crate::core::error::Result;
use crate::persist::storage::{self, SEGMENTS_DIR, TRASH_DIR};

/// 把 `segments/` 下的文件移入 `trash/`(幂等;源不存在则跳过)。
///
/// # Errors
/// rename I/O 失败时返回 [`MnemeError::Io`]。
pub(crate) fn move_to_trash(root: &Path, names: &[String]) -> Result<()> {
    storage::ensure_dir(&root.join(TRASH_DIR))?;
    for name in names {
        let from = root.join(SEGMENTS_DIR).join(name);
        if !storage::exists(&from)? {
            continue;
        }
        let to = root.join(TRASH_DIR).join(name);
        // 同名残留先删除再 rename,避免目标已存在导致失败。
        storage::remove_if_exists(&to)?;
        storage::rename(&from, &to)?;
    }
    Ok(())
}

/// 清空 `trash/`:删除其中全部文件(下次启动兜底)。
///
/// # Errors
/// 删除 I/O 失败时返回 [`MnemeError::Io`]。
pub(crate) fn purge(root: &Path) -> Result<()> {
    let dir = root.join(TRASH_DIR);
    for name in storage::list_dir(root, TRASH_DIR)? {
        storage::remove_if_exists(&dir.join(&name))?;
    }
    Ok(())
}

/// 计算 `trash/` 目录总字节数(供 `stats()` 报告)。
///
/// # Errors
/// 目录读取失败时返回 [`MnemeError::Io`]。
pub(crate) fn bytes(root: &Path) -> Result<u64> {
    let dir = root.join(TRASH_DIR);
    let mut total = 0_u64;
    for name in storage::list_dir(root, TRASH_DIR)? {
        // reason: stats 为尽力而为;单个文件元数据读取失败仅少计字节,
        // 不影响正确性(与 `Store::wal_bytes` 同口径)。
        if let Ok(metadata) = std::fs::metadata(dir.join(&name)) {
            total += metadata.len();
        }
    }
    Ok(total)
}
