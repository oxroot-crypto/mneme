//! FNV-1a 64 位哈希(`hash.rs`)。
//!
//! L0 纯函数/纯数据结构:无 I/O、无锁。为写状态的行号/键索引分片表提供**快速**
//! 哈希:键为 8–40 字节的整数与短字符串,`SipHash-1-3`(std `RandomState`)的
//! 抗 DoS 强度在本场景(进程内本地嵌入库)收益有限、常数明显更高。
//!
//! 迭代顺序与 `std::collections::HashMap` 一样不做保证;同一进程内确定。

use std::hash::{BuildHasher, Hasher};

/// FNV-1a 64 位偏移基准(规范常数)。
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 位质数(规范常数)。
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64 位哈希器。
#[derive(Debug, Clone)]
pub(crate) struct FnvHasher(u64);

impl Default for FnvHasher {
    fn default() -> Self {
        Self(FNV_OFFSET_BASIS)
    }
}

impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = self.0;
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        self.0 = hash;
    }
}

/// [`FnvHasher`] 的 `BuildHasher`(无随机种子;可复现)。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FnvBuildHasher;

impl BuildHasher for FnvBuildHasher {
    type Hasher = FnvHasher;

    fn build_hasher(&self) -> FnvHasher {
        FnvHasher::default()
    }
}

/// 计算任意可哈希值的 64 位哈希(FNV-1a;分片索引用,与 `FnvBuildHasher` 同源)。
pub(crate) fn fnv1a_hash<T: std::hash::Hash + ?Sized>(value: &T) -> u64 {
    let mut hasher = FnvHasher::default();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a 64 位规范测试向量("a" → 0xaf63dc4c8601ec8c,"foobar" → …).
    #[test]
    fn matches_fnv1a_reference_vectors() {
        let hash = |bytes: &[u8]| {
            let mut hasher = FnvHasher::default();
            hasher.write(bytes);
            hasher.finish()
        };
        assert_eq!(hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hash(b"foobar"), 0x85944171f73967e8);
    }

    /// 分片哈希与 `BuildHasher` 同源:同一键两条路径结果一致。
    #[test]
    fn direct_and_build_hasher_agree() {
        let key: u64 = 0x1234_5678_9abc_def0;
        assert_eq!(fnv1a_hash(&key), FnvBuildHasher.hash_one(key));
    }
}
