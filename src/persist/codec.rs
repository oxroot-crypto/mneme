//! L2 字节级编解码公共设施:版本门禁、CRC、带界游标与定长写入辅助。
//!
//! 这些原语被 `vsec`/`msec`/`wal`/`manifest`/`edges` 等各字节布局模块复用;
//! 从 `persist/mod.rs` 拆出,使 `mod.rs` 仅保留模块组织(设计 04 §2、§4)。

use crate::core::error::{MnemeError, Result};

/// 当前文件格式版本:高 8 位主版本、低 8 位次版本(设计 04 §2)。
///
/// 项目尚未发布,不存在需要读取的旧开发格式;任何版本差异都直接拒绝
/// (不保留旧版本读取分支),详见 `AGENTS.md`「项目状态与兼容纪律」。
pub(crate) const FORMAT_VERSION: u16 = 0x0006;

/// 校验文件格式版本必须与 `expected` 完全一致(I18)。
///
/// 项目未发布:主/次版本任何不一致都返回
/// [`MnemeError::UnsupportedVersion`],不给旧开发格式留宽容读取路径。
pub(crate) fn check_version(file: &'static str, found: u16, expected: u16) -> Result<()> {
    if found != expected {
        return Err(MnemeError::UnsupportedVersion {
            file,
            found,
            max: expected,
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

    /// 剩余未消费字节数。
    pub(crate) const fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    /// 剩余未消费字节切片(供 varint 等连续解码用)。
    pub(crate) fn rest(&self) -> &'a [u8] {
        &self.bytes[self.pos..]
    }

    /// 前移 `len` 字节并丢弃;越界返回 `Corrupted`。
    pub(crate) fn advance(&mut self, len: usize) -> Result<()> {
        self.take(len).map(|_| ())
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

    /// I18:未发布期格式版本必须精确等于当前定义,任何差异都拒绝。
    #[test]
    fn version_gate_rejects_any_mismatch() {
        assert!(check_version("vsec", FORMAT_VERSION, FORMAT_VERSION).is_ok());
        let previous = FORMAT_VERSION - 1;
        assert!(matches!(
            check_version("vsec", previous, FORMAT_VERSION),
            Err(MnemeError::UnsupportedVersion {
                file: "vsec",
                found,
                max: FORMAT_VERSION,
            }) if found == previous
        ));
        assert!(matches!(
            check_version("vsec", 0x0100, FORMAT_VERSION),
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
