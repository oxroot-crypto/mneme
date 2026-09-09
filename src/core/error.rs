//! L0 统一错误类型。
//!
//! [`MnemeError`] 是整库对外的唯一错误类型,由 `thiserror` 派生、跨层零成本传递
//! (枚举体积 = 最大变体大小,无装箱)。错误信息面向**排查**:`Corrupted` 必须带段号
//! (文件级损坏时为 `None`)与原因,`FilterParse` 必须带出错位置。
//!
//! 设计约定:**拒绝静默失败**——所有偏离形式化约束的情况都必须显式抛出结构化错误。

use crate::core::metric::Metric;
use crate::core::types::{Key, SegmentId};

/// Mneme 统一错误枚举。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MnemeError {
    /// 底层 I/O 错误。
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),
    /// 数据损坏:CRC 不过 / 魔数不符等。`segment = None` 表示文件级损坏(如 MANIFEST 全坏)。
    #[error("数据损坏(段 {segment:?}): {reason}")]
    Corrupted {
        /// 发生损坏的段;`None` 表示文件级损坏。
        segment: Option<SegmentId>,
        /// 人类可读的损坏原因。
        reason: String,
    },
    /// 向量维度与建库维度不符。
    #[error("向量维度不匹配:期望 {expected},实际 {got}")]
    DimensionMismatch {
        /// 建库时锁定的维度。
        expected: u32,
        /// 实际传入的向量长度。
        got: usize,
    },
    /// 打开时传入的度量与库中记录不符。
    #[error("度量不匹配:已有 {existing:?},请求 {requested:?}")]
    MetricMismatch {
        /// 库中已有的度量。
        existing: Metric,
        /// 调用方请求的度量。
        requested: Metric,
    },
    /// 键不存在(保留变体,当前无 API 产生,见设计 16 §4)。
    #[error("键不存在: {0}")]
    KeyNotFound(Key),
    /// 键重复(`InsertMode::RejectDuplicate` 时)。
    #[error("键重复: {0}")]
    DuplicateKey(Key),
    /// 过滤 DSL 语法错误,带位置信息。
    #[error("过滤表达式解析错误: {0}")]
    FilterParse(String),
    /// 资源忙:独占锁被占 / 备份中。
    #[error("资源忙: {0}")]
    Busy(&'static str),
    /// 参数非法(如维度超上限)。
    #[error("参数非法: {0}")]
    Invalid(&'static str),
    /// 数据超限额(见设计 16 §8)。
    #[error("字段过大: {field} 上限 {limit},实际 {got}")]
    TooLarge {
        /// 超限字段名。
        field: &'static str,
        /// 允许的上限。
        limit: usize,
        /// 实际大小。
        got: usize,
    },
    /// 文件格式主版本过新,拒绝打开(不变量 I18)。
    #[error("不支持的文件版本: {file} 发现 {found},最高支持 {max}")]
    UnsupportedVersion {
        /// 文件类型名。
        file: &'static str,
        /// 文件中的格式版本。
        found: u16,
        /// 本库支持的最高格式版本。
        max: u16,
    },
}

/// Mneme 统一结果类型。
pub type Result<T> = std::result::Result<T, MnemeError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_contains_diagnostic_context() {
        let err = MnemeError::DimensionMismatch {
            expected: 1536,
            got: 768,
        };
        let text = err.to_string();
        assert!(text.contains("1536"));
        assert!(text.contains("768"));

        let corrupted = MnemeError::Corrupted {
            segment: Some(SegmentId::new(42)),
            reason: "crc mismatch".to_string(),
        };
        assert!(corrupted.to_string().contains("42"));
        assert!(corrupted.to_string().contains("crc mismatch"));
    }

    #[test]
    fn io_error_converts_via_from() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err: MnemeError = io.into();
        assert!(matches!(err, MnemeError::Io(_)));
    }

    #[test]
    fn result_alias_works() {
        fn ok(value: u32) -> Result<u32> {
            Ok(value)
        }
        assert_eq!(ok(1).unwrap(), 1);
    }
}
