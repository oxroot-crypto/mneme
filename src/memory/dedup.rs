//! 写入期去重(`dedup.rs`)。
//!
//! 两级判重:精确级用文本 FNV-1a 64 位哈希集合($O(1)$),近似级用一次 top-1
//! 向量查询(相似度 ≥ 阈值即判重,阈值统一按余弦口径)。语义见设计 03 §6。

use crate::memory::record::{Record, RecordRef};

/// FNV-1a 64 位偏移基。
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 位素数。
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 写入期去重策略。
///
/// `Merge` 回调是**函数指针**,不能捕获宿主状态(见设计 16 §1.8)。
#[derive(Default)]
pub enum Dedup {
    /// 不去重(默认)。
    #[default]
    Off,
    /// 判重命中即拒绝,返回
    /// [`InsertOutcome::Duplicate`](crate::memory::InsertOutcome::Duplicate)。
    Reject,
    /// 判重命中则墓碑旧行、写入新行(生成新 `RowId`)。
    Replace,
    /// 判重命中仍照常插入,不返回重复信息。
    KeepBoth,
    /// 判重命中则就地合并,保留旧 `RowId`;回调返回 `None` 等价 `KeepBoth`。
    Merge(fn(&RecordRef<'_>, &RecordRef<'_>) -> Option<Record>),
}

impl std::fmt::Debug for Dedup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Dedup::Off => "Off",
            Dedup::Reject => "Reject",
            Dedup::Replace => "Replace",
            Dedup::KeepBoth => "KeepBoth",
            Dedup::Merge(_) => "Merge(..)",
        };
        f.write_str(name)
    }
}

/// 结果级去重,只作用于单次 `execute()` 的命中列表(实现见 L4)。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ResultDedup {
    /// 关闭(默认)。
    #[default]
    Off,
    /// 按 `RowId` 去重。
    ById,
    /// 按向量相似度阈值去重。
    Near {
        /// 余弦相似度阈值。
        threshold: f32,
    },
}

/// 计算文本的 FNV-1a 64 位哈希(逐字节 `xor` 后乘素数)。
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FC-MEM-POST-007(精确级判重依赖 FNV-1a 的确定性与区分度)
    #[test]
    fn fnv1a_is_deterministic_and_distinguishes() {
        assert_eq!(fnv1a64(b""), FNV_OFFSET_BASIS);
        assert_eq!(fnv1a64(b"hello"), fnv1a64(b"hello"));
        assert_ne!(fnv1a64(b"hello"), fnv1a64(b"hellp"));
    }
}
