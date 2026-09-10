//! 数据限额(见设计 16 §8)。
//!
//! 超限一律返回 `TooLarge`/`LimitExceeded`/`MetaTooDeep`,绝不静默截断。

/// 数据限额(见设计 16 §8)。超限一律返回 `TooLarge`/`LimitExceeded`/`MetaTooDeep`,绝不静默截断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// key 最大字节数(UTF-8),默认 1024。
    pub key_bytes: usize,
    /// text 最大字节数,默认 1 MiB。
    pub text_bytes: usize,
    /// metadata JSON 最大字节数,默认 64 KiB。
    pub meta_bytes: usize,
    /// metadata 最大嵌套深度,默认 32。
    pub meta_depth: u16,
    /// 命名空间最大深度,默认 32。
    pub ns_depth: u16,
    /// `top_k` 上限,默认 4096。
    pub top_k_max: u32,
    /// `ef` 上限,默认 4096。
    pub ef_max: u32,
    /// WAL 单帧 payload 上限(字节),默认 16 MiB。
    pub wal_frame_max: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            key_bytes: 1024,
            text_bytes: 1024 * 1024,
            meta_bytes: 64 * 1024,
            meta_depth: 32,
            ns_depth: 32,
            top_k_max: 4096,
            ef_max: 4096,
            wal_frame_max: 16 * 1024 * 1024,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_defaults_match_design() {
        let limits = Limits::default();
        assert_eq!(limits.key_bytes, 1024);
        assert_eq!(limits.text_bytes, 1024 * 1024);
        assert_eq!(limits.meta_bytes, 64 * 1024);
        assert_eq!(limits.meta_depth, 32);
        assert_eq!(limits.ns_depth, 32);
        assert_eq!(limits.top_k_max, 4096);
        assert_eq!(limits.ef_max, 4096);
        assert_eq!(limits.wal_frame_max, 16 * 1024 * 1024);
    }
}
