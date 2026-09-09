//! L0 基础标识类型(newtype)。
//!
//! 六类标识符全部用 newtype 包装:编译器据此区分"这个整数是行号还是序号",
//! 混用即编译错误。本模块只提供类型与构造 / 取值,不含任何 I/O 或全局状态。
//!
//! [`RowId`] 是应用可见的稳定逻辑身份,[`SlotId`] 是段内物理槽位;二者分离,
//! 使后台 compaction 重排物理位置后外部句柄依然有效(不变量 I22,见设计 02 §1)。

use std::fmt;
use std::sync::Arc;

/// 全局稳定逻辑标识:首次写入时分配,永不复用。
///
/// 跨 `update` / upsert、跨段、跨 compaction 保持不变,是公开 API 的稳定句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(u64);

impl RowId {
    /// 由原始整数构造。
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// 返回内部整数值。
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for RowId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 段内物理槽位(每物理版本一个)。
///
/// 用作 `vectors` 下标 / 删除位图 bit / HNSW 节点 id;**仅存储与索引层内部使用**,
/// 不对外暴露;段内追加写、永不复用,compaction 重建新段时按新段重新编号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotId(u32);

impl SlotId {
    /// 由原始整数构造。
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// 返回内部整数值。
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl From<u32> for SlotId {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl fmt::Display for SlotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 全局提交序号:单调递增,MVCC 快照的基石,永不复用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SeqNo(u64);

impl SeqNo {
    /// 由原始整数构造。
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// 返回内部整数值。
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for SeqNo {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for SeqNo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 段文件编号,与文件名 `seg_000042.vsec` 对应,永不复用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentId(u32);

impl SegmentId {
    /// 由原始整数构造。
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// 返回内部整数值。
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl From<u32> for SegmentId {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl fmt::Display for SegmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 命名空间编号:`(NsId, Key)` 是复合主键,WAL 帧与 `key_index` 均引用,永不复用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NsId(u32);

impl NsId {
    /// 由原始整数构造。
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// 返回内部整数值。
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl From<u32> for NsId {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl fmt::Display for NsId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 用户外部键(可选),即应用层的记忆 id。
///
/// 内部用 [`Arc<str>`] 存储,克隆仅复制引用计数,便于跨段共享。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(Arc<str>);

impl Key {
    /// 由字符串构造。
    ///
    /// # Arguments
    ///
    /// * `value` - 任意可转换为 [`Arc<str>`] 的字符串(如 `&str`、`String`)。
    ///
    /// 长度限额在写入入口校验(见设计 16 §8),本构造函数不做校验。
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    /// 以 `&str` 形式返回内部值。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Key {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl From<String> for Key {
    fn from(value: String) -> Self {
        Self(Arc::from(value))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newtype_roundtrips_raw_value() {
        assert_eq!(RowId::new(7).get(), 7);
        assert_eq!(SlotId::new(9).get(), 9);
        assert_eq!(SeqNo::new(11).get(), 11);
        assert_eq!(SegmentId::new(3).get(), 3);
        assert_eq!(NsId::new(5).get(), 5);
    }

    #[test]
    fn distinct_newtypes_are_not_interchangeable() {
        // 编译期即可区分;此测试确保类型仍可比较与排序。
        assert!(RowId::new(1) < RowId::new(2));
        assert_eq!(RowId::from(42_u64), RowId::new(42));
    }

    #[test]
    fn key_from_str_and_string_share_value() {
        let from_str = Key::from("mem_001");
        let from_string = Key::from(String::from("mem_001"));
        assert_eq!(from_str, from_string);
        assert_eq!(from_str.as_str(), "mem_001");
        assert_eq!(from_str.to_string(), "mem_001");
    }

    #[test]
    fn key_clone_is_cheap_and_equal() {
        let key = Key::new("a");
        let cloned = key.clone();
        assert_eq!(key, cloned);
    }
}
