//! `trash` 延迟删除(设计 04 §9、12 §3)。
//!
//! Windows 不允许删除被 mmap/句柄打开的文件:提交新 MANIFEST 后先把旧段文件
//! `rename` 进 `trash/`(rename 对打开中的文件是允许的),待最后一个读者释放后
//! 或下次启动时物理删除。所有操作经 [`Storage`] 后端(内存后端同样可用)。

use crate::core::error::Result;
use crate::persist::storage::{SEGMENTS_DIR, Storage, TRASH_DIR};

/// 把 `segments/` 下的文件移入 `trash/`(幂等;源不存在则跳过)。
///
/// # Errors
/// rename I/O 失败时返回 [`crate::MnemeError::Io`]。
pub(crate) fn move_to_trash(storage: &dyn Storage, names: &[String]) -> Result<()> {
    storage.ensure_dir(TRASH_DIR)?;
    for name in names {
        let from = format!("{SEGMENTS_DIR}/{name}");
        if !storage.exists(&from)? {
            continue;
        }
        let to = format!("{TRASH_DIR}/{name}");
        // 同名残留先删除再 rename,避免目标已存在导致失败。
        storage.remove_if_exists(&to)?;
        storage.rename(&from, &to)?;
    }
    Ok(())
}

/// 清空 `trash/`:删除其中全部文件(下次启动兜底)。
///
/// # Errors
/// 删除 I/O 失败时返回 [`crate::MnemeError::Io`]。
pub(crate) fn purge(storage: &dyn Storage) -> Result<()> {
    for name in storage.list_dir(TRASH_DIR)? {
        storage.remove_if_exists(&format!("{TRASH_DIR}/{name}"))?;
    }
    Ok(())
}

/// 计算 `trash/` 目录总字节数(供 `stats()` 报告)。
///
/// # Errors
/// 目录读取失败时返回 [`crate::MnemeError::Io`]。
pub(crate) fn bytes(storage: &dyn Storage) -> Result<u64> {
    let mut total = 0_u64;
    for name in storage.list_dir(TRASH_DIR)? {
        // reason: stats 为尽力而为;单个文件元数据读取失败仅少计字节,
        // 不影响正确性(与 `Store::wal_bytes` 同口径)。
        if let Ok(meta) = storage.stat(&format!("{TRASH_DIR}/{name}")) {
            total += meta.len;
        }
    }
    Ok(total)
}
