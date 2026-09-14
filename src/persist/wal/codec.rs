//! WAL 文件头 / 帧的编解码与回放遍历(`wal/codec.rs`)。

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::persist::vsec::{metric_from_u8, metric_to_u8};
use crate::persist::{FORMAT_VERSION, check_version, crc32};

use super::{FILE_HEADER_LEN, FRAME_HEADER_LEN, FrameKind, MAGIC, WalHeader};
#[cfg(test)]
use super::{Frame, Replay};

/// 编码 WAL 文件头(32 字节,CRC 覆盖 `[0,12)`)。
pub(crate) fn encode_file_header(dimension: u32, metric: Metric) -> [u8; FILE_HEADER_LEN] {
    let mut out = [0_u8; FILE_HEADER_LEN];
    out[0..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    out[6..10].copy_from_slice(&dimension.to_le_bytes());
    out[10] = metric_to_u8(metric);
    let crc = crc32(&out[0..12]);
    out[12..16].copy_from_slice(&crc.to_le_bytes());
    out
}

/// 校验并解析 WAL 文件头。
///
/// # Errors
/// 魔数/版本/CRC 不符或文件短于头部时返回 [`MnemeError::Corrupted`]。
pub(crate) fn parse_file_header(bytes: &[u8]) -> Result<WalHeader> {
    if bytes.len() < FILE_HEADER_LEN {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "wal: 文件短于头部".to_string(),
        });
    }
    if bytes[0..4] != MAGIC {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "wal: 魔数不符".to_string(),
        });
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    check_version("wal", version, FORMAT_VERSION)?;
    let stored_crc = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    if crc32(&bytes[0..12]) != stored_crc {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "wal: header_crc32 不符".to_string(),
        });
    }
    let dimension = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);
    let metric = metric_from_u8(bytes[10])?;
    Ok(WalHeader { dimension, metric })
}

/// 编码一帧。
pub(crate) fn encode_frame(seqno: u64, kind: FrameKind, payload: &[u8]) -> Vec<u8> {
    // 单帧负载有界(记录体限额:文本 1 MiB + 向量 ≤ 512 KiB + metadata 64 KiB),
    // 远小于 u32::MAX,转换可证明不会失败;静默截断会破坏帧布局。
    let payload_len =
        u32::try_from(payload.len()).expect("WAL 单帧负载远小于 u32::MAX(FC-GLOBAL-PRE-003 限额)");
    // 单缓冲:先占 CRC 位,被覆盖区连续写入后一次回填 CRC,免中间缓冲再整段拷出。
    let mut out = Vec::with_capacity(4 + 4 + 8 + 1 + payload.len());
    out.extend_from_slice(&[0_u8; 4]);
    let covered_start = out.len();
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.extend_from_slice(&seqno.to_le_bytes());
    out.push(kind.as_u8());
    out.extend_from_slice(payload);
    let crc = crc32(&out[covered_start..]);
    out[..4].copy_from_slice(&crc.to_le_bytes());
    out
}

/// 回放 WAL:校验文件头后逐帧解析。
///
/// # Errors
/// 文件头损坏,或遇到未知帧类型(FC-PERSIST-ERR-001,绝不静默跳过)时返回错误。
/// 尾部撕裂帧(长度不足/CRC 不符)不计入 `frames` 并停止回放,由调用方截断。
#[cfg(test)]
pub(crate) fn replay(bytes: &[u8]) -> Result<Replay> {
    let mut frames = Vec::new();
    let valid_len = visit_frames(bytes, |seqno, kind, payload, _end| {
        frames.push(Frame {
            seqno,
            kind,
            payload: payload.to_vec(),
        });
        Ok(())
    })?;
    Ok(Replay { frames, valid_len })
}

/// 流式遍历 WAL 帧,每帧以 `on_frame(seqno, kind, payload, frame_end)` 回调,不整体
/// 物化(空间 `O(1)`,批缓冲由调用方自理;设计 04 §3.4)。`frame_end` 为该帧结束的
/// 绝对字节偏移,便于调用方记录「已提交位置」并截断未提交尾部。返回有效字节长度。
///
/// # Errors
/// 文件头损坏或未知帧类型(FC-PERSIST-ERR-001)时返回结构化错误;
/// 尾部撕裂帧(长度不足/CRC 不符)停止遍历并返回其前长度。
pub(crate) fn visit_frames(
    bytes: &[u8],
    mut on_frame: impl FnMut(u64, FrameKind, &[u8], usize) -> Result<()>,
) -> Result<usize> {
    parse_file_header(bytes)?;
    let mut offset = FILE_HEADER_LEN;
    while offset < bytes.len() {
        // 帧头不足 → 撕裂尾部,停止。
        if offset + FRAME_HEADER_LEN > bytes.len() {
            break;
        }
        let payload_len = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        let frame_end = offset + FRAME_HEADER_LEN + payload_len;
        if frame_end > bytes.len() {
            break;
        }
        let stored_crc = u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]);
        let covered = &bytes[offset + 4..frame_end];
        if crc32(covered) != stored_crc {
            break;
        }
        let seqno = u64::from_le_bytes(bytes[offset + 8..offset + 16].try_into().unwrap_or([0; 8]));
        let kind_byte = bytes[offset + 16];
        let Some(kind) = FrameKind::from_u8(kind_byte) else {
            return Err(MnemeError::Corrupted {
                segment: None,
                reason: format!("wal: 未知帧类型 {kind_byte}"),
            });
        };
        let payload = &bytes[offset + FRAME_HEADER_LEN..frame_end];
        on_frame(seqno, kind, payload, frame_end)?;
        offset = frame_end;
    }
    Ok(offset)
}

impl FrameKind {
    /// 帧类型编号。
    pub(crate) const fn as_u8(self) -> u8 {
        match self {
            FrameKind::Insert => 1,
            FrameKind::Delete => 2,
            FrameKind::Touch => 3,
            FrameKind::Checkpoint => 4,
            FrameKind::BatchBegin => 5,
            FrameKind::BatchCommit => 6,
            FrameKind::DeleteRow => 7,
            FrameKind::TouchRow => 8,
            FrameKind::NsRegister => 9,
            FrameKind::Update => 10,
            FrameKind::UpdateRow => 11,
            FrameKind::Relate => 12,
            FrameKind::Unrelate => 13,
            FrameKind::RelKindRegister => 14,
            FrameKind::NsUnregister => 15,
        }
    }

    /// 由编号解析;未知编号返回 `None`。
    pub(crate) const fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            1 => FrameKind::Insert,
            2 => FrameKind::Delete,
            3 => FrameKind::Touch,
            4 => FrameKind::Checkpoint,
            5 => FrameKind::BatchBegin,
            6 => FrameKind::BatchCommit,
            7 => FrameKind::DeleteRow,
            8 => FrameKind::TouchRow,
            9 => FrameKind::NsRegister,
            10 => FrameKind::Update,
            11 => FrameKind::UpdateRow,
            12 => FrameKind::Relate,
            13 => FrameKind::Unrelate,
            14 => FrameKind::RelKindRegister,
            15 => FrameKind::NsUnregister,
            _ => return None,
        })
    }
}
