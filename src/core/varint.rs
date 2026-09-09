//! L0 变长整数(varint)编解码。
//!
//! 把整数的二进制按 **7 位一组**从低位切分,低位组在前;除最后一组外,每组所在字节
//! 的最高位(continuation bit)置 1。小整数(0–127)占 1 字节,128–16383 占 2 字节,
//! 以此类推,u64 最坏 10 字节。
//!
//! 用于倒排表 `SlotId` 差分与段内变长字段长度前缀。**畸形输入返回结构化错误,
//! 绝不 panic、绝不静默跳过**(不变量 FC-CORE-ERR-001)。

use crate::core::error::{MnemeError, Result};

/// 将 `u64` 以 varint 追加到 `out`。
///
/// # Arguments
///
/// * `value` - 待编码的整数。
/// * `out` - 输出缓冲区,按低位组在前追加字节。
///
/// # Examples
///
/// ```
/// use mneme::varint::{decode_u64, encode_u64};
///
/// let mut buf = Vec::new();
/// encode_u64(300, &mut buf);
/// assert_eq!(buf, vec![0xAC, 0x02]);
/// assert_eq!(decode_u64(&buf).unwrap(), (300, 2));
/// ```
pub fn encode_u64(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push(((value as u8) & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// 将 `u32` 以 varint 追加到 `out`。
///
/// # Arguments
///
/// * `value` - 待编码的整数。
/// * `out` - 输出缓冲区,按低位组在前追加字节。
pub fn encode_u32(mut value: u32, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push(((value as u8) & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// 从 `input` 解码一个 varint 编码的 `u64`。
///
/// # Returns
///
/// 成功时返回 `(解码值, 消耗字节数)`。
///
/// # Errors
///
/// * [`MnemeError::Corrupted`] —— 输入被截断(末尾仍带 continuation bit)或超过
///   `u64` 的最长 10 字节表示(数值溢出)。
///
/// # Examples
///
/// ```
/// use mneme::varint::decode_u64;
///
/// assert_eq!(decode_u64(&[0x01]).unwrap(), (1, 1));
/// assert!(decode_u64(&[0x80]).is_err());
/// ```
pub fn decode_u64(input: &[u8]) -> Result<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    for (index, &byte) in input.iter().enumerate() {
        let low = u64::from(byte & 0x7f);
        if shift == 63 && low > 1 {
            return Err(corrupted("varint 数值溢出 u64"));
        }
        if shift > 63 {
            return Err(corrupted("varint 超过 u64 最长 10 字节"));
        }
        result |= low << shift;
        if byte & 0x80 == 0 {
            return Ok((result, index + 1));
        }
        shift += 7;
    }
    Err(corrupted("varint 输入被截断"))
}

/// 从 `input` 解码一个 varint 编码的 `u32`。
///
/// # Returns
///
/// 成功时返回 `(解码值, 消耗字节数)`。
///
/// # Errors
///
/// * [`MnemeError::Corrupted`] —— 输入被截断或超过 `u32` 的最长 5 字节表示(数值溢出)。
///
/// # Examples
///
/// ```
/// use mneme::varint::decode_u32;
///
/// assert_eq!(decode_u32(&[0x7f]).unwrap(), (127, 1));
/// assert!(decode_u32(&[0x80, 0x80, 0x80, 0x80, 0x10]).is_err());
/// ```
pub fn decode_u32(input: &[u8]) -> Result<(u32, usize)> {
    let mut result: u32 = 0;
    let mut shift: u32 = 0;
    for (index, &byte) in input.iter().enumerate() {
        let low = u32::from(byte & 0x7f);
        if shift == 28 && low > 0x0f {
            return Err(corrupted("varint 数值溢出 u32"));
        }
        if shift > 28 {
            return Err(corrupted("varint 超过 u32 最长 5 字节"));
        }
        result |= low << shift;
        if byte & 0x80 == 0 {
            return Ok((result, index + 1));
        }
        shift += 7;
    }
    Err(corrupted("varint 输入被截断"))
}

/// 构造一个文件级损坏错误。
fn corrupted(reason: &str) -> MnemeError {
    MnemeError::Corrupted {
        segment: None,
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(value: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        encode_u64(value, &mut buf);
        buf
    }

    #[test]
    fn encodes_small_values_in_one_byte() {
        assert_eq!(encode(0), vec![0x00]);
        assert_eq!(encode(1), vec![0x01]);
        assert_eq!(encode(127), vec![0x7f]);
    }

    #[test]
    fn encodes_300_as_documented() {
        assert_eq!(encode(300), vec![0xAC, 0x02]);
    }

    #[test]
    fn roundtrips_boundary_values() {
        for value in [
            0_u64,
            1,
            127,
            128,
            300,
            16_383,
            16_384,
            u32::MAX as u64,
            u64::MAX,
        ] {
            let buf = encode(value);
            assert_eq!(decode_u64(&buf).unwrap(), (value, buf.len()));
        }
    }

    #[test]
    fn u32_roundtrips_boundary_values() {
        for value in [0_u32, 1, 127, 128, 16_383, 16_384, u32::MAX] {
            let mut buf = Vec::new();
            encode_u32(value, &mut buf);
            assert_eq!(decode_u32(&buf).unwrap(), (value, buf.len()));
        }
    }

    #[test]
    fn decode_reports_bytes_consumed_with_trailing_data() {
        let mut buf = vec![0xAC, 0x02];
        buf.extend_from_slice(&[0xFF, 0xFF]);
        assert_eq!(decode_u64(&buf).unwrap(), (300, 2));
    }

    #[test]
    fn truncated_input_is_rejected() {
        assert!(matches!(
            decode_u64(&[0x80]),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    #[test]
    fn overlong_u64_is_rejected() {
        let too_long = [0x80_u8; 11];
        assert!(matches!(
            decode_u64(&too_long),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    #[test]
    fn overflowing_tenth_byte_is_rejected() {
        // 第 10 字节只允许 0 或 1,否则 u64 溢出。
        let mut buf = vec![0x80_u8; 9];
        buf.push(0x02);
        assert!(matches!(
            decode_u64(&buf),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    #[test]
    fn overflowing_fifth_u32_byte_is_rejected() {
        let buf = [0x80_u8, 0x80, 0x80, 0x80, 0x10];
        assert!(matches!(
            decode_u32(&buf),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(decode_u64(&[]).is_err());
        assert!(decode_u32(&[]).is_err());
    }
}
