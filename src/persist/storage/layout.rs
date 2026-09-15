//! 目录布局常量、文件名与路径安全解析。

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
