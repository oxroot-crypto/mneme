//! msec 轻量索引区共享个常量同解码辅助(错误构造、带 `u32` 长度前缀个 UTF-8 读取)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::persist::Cursor;

/// 字段字典条目数硬上限(id 为 `u16`,防御恶意文件声明巨量条目)。
pub(super) const MAX_FIELDS: usize = u16::MAX as usize + 1;

/// 构造结构损坏错误。
pub(in crate::persist::msec) fn corrupted(reason: &str) -> MnemeError {
    MnemeError::Corrupted {
        segment: None,
        reason: reason.to_string(),
    }
}

/// 读取带 `u32` 长度前缀的 UTF-8 字符串。
pub(in crate::persist::msec) fn read_utf8(cursor: &mut Cursor<'_>, what: &str) -> Result<Arc<str>> {
    let len = cursor.u32()? as usize;
    let text = std::str::from_utf8(cursor.take(len)?)
        .map_err(|_| corrupted(&format!("{what}: 非法 UTF-8")))?;
    Ok(Arc::from(text))
}
