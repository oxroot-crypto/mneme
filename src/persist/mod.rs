//! L2 持久层:文件字节布局、WAL、段文件、MANIFEST 与崩溃恢复。
//!
//! 本层把 L1 的内存引擎升级为"重启不丢数据、可崩溃恢复"的本地库:
//! 定义 `vsec`(向量段)/`msec`(元数据段)/`WAL`/`MANIFEST` 的**字节级格式**,
//! 实现追加式 WAL、write-once MANIFEST、段级 CRC 与统一可见性合并。
//!
//! # 模块
//!
//! * `vsec` —— 向量段编解码(头部 + 向量区 + norm 区 + 删除位图 + payload CRC)。
//! * `msec` —— 元数据段编解码(头部 + 记录体 + 版本链 + key 索引)。
//! * `delta` —— 跨段覆盖区(墓碑/更新/访问)编解码。
//! * `edges` —— 关系邻接索引编解码。
//! * `wal` —— WAL 帧编解码、回放与写入器。
//! * `manifest` —— MANIFEST 编解码(write-once + `current` 指针)。
//! * `source` —— 段读取后端抽象(`SegmentSource` 与 `FileSource`)。
//! * `storage` —— 目录布局、原子写入与独占文件锁。
//! * `trash` —— 旧段文件的延迟删除。
//! * `flush` —— 写状态 → 段文件(全量快照)。
//! * `recover` —— 段文件 + WAL → 写状态。
//! * `store` —— 协调句柄 `Store`(实现 `PersistHook`、`flush`、`open`)。
//!
//! > 本层向下依赖 [`crate::core`] 与 [`crate::memory`](L1,L2 高于 L1);
//! > `memmap2`/`MmapSource` 按依赖白名单自 L3 引入,本层仅提供 `FileSource`。

pub(crate) mod delta;
pub(crate) mod edges;
pub(crate) mod flush;
pub(crate) mod manifest;
pub(crate) mod msec;
pub(crate) mod recover;
pub(crate) mod source;
pub(crate) mod storage;
pub(crate) mod store;
pub(crate) mod trash;
pub(crate) mod vsec;
pub(crate) mod wal;

use crate::core::error::{MnemeError, Result};

/// 当前支持的文件格式版本:高 8 位主版本、低 8 位次版本(设计 04 §2、§12)。
pub(crate) const FORMAT_VERSION: u16 = 0x0001;

/// 库支持的最大主版本;`major(found) > MAX_MAJOR` 时拒绝打开(I18)。
pub(crate) const MAX_MAJOR: u8 = 0x00;

/// 取出版本号的主版本(高 8 位)。
pub(crate) const fn major(version: u16) -> u8 {
    (version >> 8) as u8
}

/// 校验文件主版本是否可读;更高主版本返回 [`MnemeError::UnsupportedVersion`](I18)。
///
/// # Errors
/// `major(found) > MAX_MAJOR` 时返回 [`MnemeError::UnsupportedVersion`]。
pub(crate) fn check_version(file: &'static str, found: u16) -> Result<()> {
    if major(found) > MAX_MAJOR {
        return Err(MnemeError::UnsupportedVersion {
            file,
            found,
            max: FORMAT_VERSION,
        });
    }
    Ok(())
}

/// 把 `value` 向上对齐到 `align` 的整数倍(`align` 必须是 2 的幂且非 0)。
pub(crate) const fn align_up(value: usize, align: usize) -> usize {
    value.div_ceil(align) * align
}

/// 计算 CRC-32/IEEE(设计 04 §4)。
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// 读取字节缓冲的带界游标;越界或长度不符返回结构化 [`MnemeError::Corrupted`],
/// 绝不 panic(对齐 L0 的 varint 解码口径,FC-CORE-ERR-001)。
pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
    context: &'static str,
}

impl<'a> Cursor<'a> {
    /// 基于整段字节与上下文名(用于错误定位)新建游标。
    pub(crate) fn new(bytes: &'a [u8], context: &'static str) -> Self {
        Self {
            bytes,
            pos: 0,
            context,
        }
    }

    /// 当前读取位置(已消费字节数)。
    pub(crate) const fn position(&self) -> usize {
        self.pos
    }

    /// 剩余未消费字节数。
    pub(crate) const fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    /// 是否已读到末尾。
    pub(crate) const fn is_empty(&self) -> bool {
        self.pos == self.bytes.len()
    }

    fn corrupt(&self, reason: &'static str) -> MnemeError {
        MnemeError::Corrupted {
            segment: None,
            reason: format!("{}: {reason}(偏移 {})", self.context, self.pos),
        }
    }

    /// 读取 `len` 字节切片;越界返回 `Corrupted`。
    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| self.corrupt("长度溢出"))?;
        if end > self.bytes.len() {
            return Err(self.corrupt("读取越界"));
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// 读取 1 字节。
    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    /// 读取小端 `u16`。
    pub(crate) fn u16(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// 读取小端 `u32`。
    pub(crate) fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// 读取小端 `u64`。
    pub(crate) fn u64(&mut self) -> Result<u64> {
        let bytes = self.take(8)?;
        let mut array = [0_u8; 8];
        array.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(array))
    }

    /// 读取小端 `i64`。
    pub(crate) fn i64(&mut self) -> Result<i64> {
        Ok(self.u64()? as i64)
    }
}

/// 把定长字段写入 `out`(小端辅助函数,避免各处重复)。
pub(crate) fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// 写入小端 `u32`。
pub(crate) fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// 写入小端 `u64`。
pub(crate) fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// 写入小端 `i64`。
pub(crate) fn put_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// 写入带 `u32` 长度前缀的字节串。
pub(crate) fn put_bytes_u32(out: &mut Vec<u8>, bytes: &[u8]) {
    put_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_gate_rejects_higher_major() {
        assert!(check_version("vsec", FORMAT_VERSION).is_ok());
        // 同主版本、更高次版本可按"忽略未知可选字段"读取。
        assert!(check_version("vsec", 0x00FF).is_ok());
        assert!(matches!(
            check_version("vsec", 0x0100),
            Err(MnemeError::UnsupportedVersion {
                file: "vsec",
                found: 0x0100,
                max: FORMAT_VERSION,
            })
        ));
    }

    #[test]
    fn cursor_reads_little_endian_and_rejects_truncation() {
        let bytes = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0xAA];
        let mut cursor = Cursor::new(&bytes, "test");
        assert_eq!(cursor.u32().expect("u32"), 0x0403_0201);
        assert_eq!(cursor.u32().expect("u32"), 0x0807_0605);
        assert_eq!(cursor.u8().expect("u8"), 0xAA);
        assert!(cursor.is_empty());
        let mut short = Cursor::new(&bytes[..2], "test");
        assert!(matches!(short.u32(), Err(MnemeError::Corrupted { .. })));
    }
}
