//! WAL 文件命名、列举与序号解析。

use crate::core::error::Result;
// `MnemeError` 只出现在 `wal_files` 的 rustdoc 链接里;放行 unused 警告。
#[allow(unused_imports)]
use crate::core::error::MnemeError;
use crate::persist::storage::{Storage, WAL_DIR};

/// 活动 WAL 文件相对路径(按文件序号,`000001` 起)。
pub(in crate::persist::store) fn wal_name(index: u32) -> String {
    format!("{WAL_DIR}/wal_{index:06}.log")
}

/// 列出全部 WAL 文件相对路径(按文件序号升序)。
///
/// # Errors
/// 目录列举失败时返回 [`MnemeError::Io`]。
pub(in crate::persist::store) fn wal_files(storage: &dyn Storage) -> Result<Vec<String>> {
    let mut names: Vec<String> = storage
        .list_dir(WAL_DIR)?
        .into_iter()
        .filter(|name| name.starts_with("wal_") && name.ends_with(".log"))
        .map(|name| format!("{WAL_DIR}/{name}"))
        .collect();
    names.sort();
    Ok(names)
}

/// 从 WAL 文件名解析文件序号(严格 `wal_NNNNNN.log`;不匹配返回 `None`)。
pub(in crate::persist::store) fn wal_index_of(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("wal_")?.strip_suffix(".log")?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}
