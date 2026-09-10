//! WAL 帧编解码与回放(设计 04 §2.3、§3)。
//!
//! 文件 = 32 字节定长头 + 帧序列。每帧:
//! `[u32 crc32][u32 payload_len][u64 seqno][u8 type][payload]`,CRC 覆盖
//! `[payload_len, seqno, type, payload]`(即本帧除自身 CRC 外的全部字节)。
//!
//! 回放时若读不满 `payload_len` 或 CRC 不符 → 该帧及其后全部丢弃(只追加文件,
//! 尾部之后不可能是有效数据);未知帧类型 → **报错而非静默跳过**(FC-PERSIST-ERR-001)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::meta::{self, Meta};
use crate::core::metric::Metric;
use crate::persist::msec::{self, EntryData};
use crate::persist::vsec::{metric_from_u8, metric_to_u8};
use crate::persist::{
    Cursor, FORMAT_VERSION, check_version, crc32, put_bytes_u32, put_i64, put_u32, put_u64,
};

/// WAL 魔数。
pub(crate) const MAGIC: [u8; 4] = *b"WAL1";
/// WAL 文件头长度(字节)。
pub(crate) const FILE_HEADER_LEN: usize = 32;
/// 帧头长度:`crc(4) + payload_len(4) + seqno(8) + type(1)`。
pub(crate) const FRAME_HEADER_LEN: usize = 17;

/// WAL 文件头字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WalHeader {
    /// 建库维度。
    pub(crate) dimension: u32,
    /// 距离度量。
    pub(crate) metric: Metric,
}

/// 帧类型(设计 04 §2.3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameKind {
    /// 插入记录体。
    Insert,
    /// 按 `(ns, key)` 删除。
    Delete,
    /// 访问统计更新。
    Touch,
    /// 检查点(水位)。
    Checkpoint,
    /// 批开始。
    BatchBegin,
    /// 批提交。
    BatchCommit,
    /// 按 `RowId` 删除。
    DeleteRow,
    /// 按 `RowId` 访问更新。
    TouchRow,
    /// 命名空间注册。
    NsRegister,
    /// 按 `(ns, key)` 局部更新。
    Update,
    /// 按 `RowId` 局部更新。
    UpdateRow,
    /// 建立关系。
    Relate,
    /// 删除关系。
    Unrelate,
    /// 关系类型注册。
    RelKindRegister,
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
            _ => return None,
        })
    }
}

/// 一帧 WAL 记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    /// 全局单调写序号。
    pub(crate) seqno: u64,
    /// 帧类型。
    pub(crate) kind: FrameKind,
    /// 帧负载(原始字节;解释见各 `encode_*`/`decode_*`)。
    pub(crate) payload: Vec<u8>,
}

/// 回放结果:有效帧 + 有效字节长度(撕裂帧之前的长度)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Replay {
    /// 完整且 CRC 正确的帧序列。
    pub(crate) frames: Vec<Frame>,
    /// 头部 + 有效帧的总字节数;小于文件长度表示尾部有撕裂帧。
    pub(crate) valid_len: usize,
}

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
    check_version("wal", version)?;
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
    let payload_len = payload.len() as u32;
    let mut covered = Vec::with_capacity(4 + 8 + 1 + payload.len());
    covered.extend_from_slice(&payload_len.to_le_bytes());
    covered.extend_from_slice(&seqno.to_le_bytes());
    covered.push(kind.as_u8());
    covered.extend_from_slice(payload);
    let mut out = Vec::with_capacity(4 + covered.len());
    out.extend_from_slice(&crc32(&covered).to_le_bytes());
    out.extend_from_slice(&covered);
    out
}

/// 回放 WAL:校验文件头后逐帧解析。
///
/// # Errors
/// 文件头损坏,或遇到未知帧类型(FC-PERSIST-ERR-001,绝不静默跳过)时返回错误。
/// 尾部撕裂帧(长度不足/CRC 不符)不计入 `frames` 并停止回放,由调用方截断。
pub(crate) fn replay(bytes: &[u8]) -> Result<Replay> {
    let mut frames = Vec::new();
    let valid_len = visit_frames(bytes, |seqno, kind, payload| {
        frames.push(Frame {
            seqno,
            kind,
            payload: payload.to_vec(),
        });
        Ok(())
    })?;
    Ok(Replay { frames, valid_len })
}

/// 流式遍历 WAL 帧,每帧以 `on_frame(seqno, kind, payload)` 回调,不整体物化
/// (空间 `O(1)`,批缓冲由调用方自理;设计 04 §3.4)。返回有效字节长度。
///
/// # Errors
/// 文件头损坏或未知帧类型(FC-PERSIST-ERR-001)时返回结构化错误;
/// 尾部撕裂帧(长度不足/CRC 不符)停止遍历并返回其前长度。
pub(crate) fn visit_frames(
    bytes: &[u8],
    mut on_frame: impl FnMut(u64, FrameKind, &[u8]) -> Result<()>,
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
        on_frame(seqno, kind, payload)?;
        offset = frame_end;
    }
    Ok(offset)
}

/// 编码 `Insert` 负载:`[记录体(含长度前缀)][u32 dim][f32 × dim]`。
///
/// 记录体(设计 04 §2.3)只含元数据不含向量,故 WAL 追加向量副本以支持崩溃恢复;
/// 该字段是对设计 §2.3 的必要补全(否则未 flush 的记录重启后向量丢失)。
///
/// # Errors
/// 记录体或向量长度超限时返回 [`MnemeError::TooLarge`]。
pub(crate) fn encode_insert(entry: &EntryData, vector: &[f32]) -> Result<Vec<u8>> {
    let mut out = msec::encode_entry(entry)?;
    let dim = u32::try_from(vector.len()).map_err(|_| MnemeError::TooLarge {
        field: "wal insert vector",
        limit: u32::MAX as usize,
        got: vector.len(),
    })?;
    put_u32(&mut out, dim);
    for value in vector {
        out.extend_from_slice(&value.to_le_bytes());
    }
    Ok(out)
}

/// 解码 `Insert` 负载,返回 `(记录体, 向量)`。
///
/// # Errors
/// 记录体或向量损坏时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_insert(payload: &[u8]) -> Result<(EntryData, Vec<f32>)> {
    let entry = msec::entry_from_prefix(payload)?;
    let total_len = u32::from_le_bytes(payload[0..4].try_into().unwrap_or([0; 4])) as usize;
    let mut cursor = Cursor::new(&payload[4 + total_len..], "wal insert 向量");
    let dim = cursor.u32()? as usize;
    let mut vector = Vec::with_capacity(dim);
    for _ in 0..dim {
        vector.push(f32::from_le_bytes(
            cursor.take(4)?.try_into().unwrap_or([0; 4]),
        ));
    }
    Ok((entry, vector))
}

/// 编码 `Delete` 负载 `[NsId u32][key len+bytes]`。
pub(crate) fn encode_delete(ns_id: u32, key: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, ns_id);
    put_bytes_u32(&mut out, key.as_bytes());
    out
}

/// 解码 `Delete` 负载。
///
/// # Errors
/// 长度越界或 key 非 UTF-8 时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_delete(payload: &[u8]) -> Result<(u32, Arc<str>)> {
    let mut cursor = Cursor::new(payload, "wal delete");
    let ns_id = cursor.u32()?;
    let len = cursor.u32()? as usize;
    let key = std::str::from_utf8(cursor.take(len)?).map_err(|error| MnemeError::Corrupted {
        segment: None,
        reason: format!("wal: key 非 UTF-8:{error}"),
    })?;
    Ok((ns_id, Arc::from(key)))
}

/// 编码 `DeleteRow` 负载。
pub(crate) fn encode_delete_row(rowid: u64) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, rowid);
    out
}

/// 解码 `DeleteRow` 负载。
///
/// # Errors
/// 长度不足时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_delete_row(payload: &[u8]) -> Result<u64> {
    let mut cursor = Cursor::new(payload, "wal delete_row");
    cursor.u64()
}

/// 编码 `TouchRow` 负载 `[RowId][i64 at_ms][u32 access_delta][f32 importance_delta]`。
pub(crate) fn encode_touch_row(
    rowid: u64,
    at_ms: i64,
    access_delta: u32,
    importance_delta: f32,
) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, rowid);
    put_i64(&mut out, at_ms);
    put_u32(&mut out, access_delta);
    out.extend_from_slice(&importance_delta.to_le_bytes());
    out
}

/// 解码 `TouchRow` 负载。
///
/// # Errors
/// 长度不足时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_touch_row(payload: &[u8]) -> Result<(u64, i64, u32, f32)> {
    let mut cursor = Cursor::new(payload, "wal touch_row");
    let rowid = cursor.u64()?;
    let at_ms = cursor.i64()?;
    let access_delta = cursor.u32()?;
    let importance_delta = f32::from_le_bytes(cursor.take(4)?.try_into().unwrap_or([0; 4]));
    Ok((rowid, at_ms, access_delta, importance_delta))
}

/// 编码 `Relate` 负载 `[from][to][kind u16][f32 weight][meta len+bytes]`。
pub(crate) fn encode_relate(from: u64, to: u64, kind: u16, weight: f32, meta: &Meta) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, from);
    put_u64(&mut out, to);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&weight.to_le_bytes());
    put_bytes_u32(&mut out, &meta::to_bytes(meta));
    out
}

/// 解码 `Relate` 负载。
///
/// # Errors
/// 长度越界或 meta 损坏时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_relate(payload: &[u8]) -> Result<(u64, u64, u16, f32, Meta)> {
    let mut cursor = Cursor::new(payload, "wal relate");
    let from = cursor.u64()?;
    let to = cursor.u64()?;
    let kind = cursor.u16()?;
    let weight = f32::from_le_bytes(cursor.take(4)?.try_into().unwrap_or([0; 4]));
    let meta_len = cursor.u32()? as usize;
    let meta = meta::from_bytes(cursor.take(meta_len)?)?;
    Ok((from, to, kind, weight, meta))
}

/// 编码 `Unrelate` 负载 `[from][to][kind u16]`。
pub(crate) fn encode_unrelate(from: u64, to: u64, kind: u16) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, from);
    put_u64(&mut out, to);
    out.extend_from_slice(&kind.to_le_bytes());
    out
}

/// 解码 `Unrelate` 负载。
///
/// # Errors
/// 长度不足时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_unrelate(payload: &[u8]) -> Result<(u64, u64, u16)> {
    let mut cursor = Cursor::new(payload, "wal unrelate");
    Ok((cursor.u64()?, cursor.u64()?, cursor.u16()?))
}

/// 编码 `Checkpoint` 负载。
pub(crate) fn encode_checkpoint(watermark_seqno: u64) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, watermark_seqno);
    out
}

/// 解码 `Checkpoint` 负载。
///
/// # Errors
/// 长度不足时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_checkpoint(payload: &[u8]) -> Result<u64> {
    let mut cursor = Cursor::new(payload, "wal checkpoint");
    cursor.u64()
}

/// 编码 `BatchBegin` 负载。
pub(crate) fn encode_batch_begin(count: u32) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, count);
    out
}

/// 编码 `BatchCommit` 负载 `[count][batch_crc]`。
pub(crate) fn encode_batch_commit(count: u32, batch_crc: u32) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, count);
    put_u32(&mut out, batch_crc);
    out
}

/// 解码 `BatchCommit` 负载。
///
/// # Errors
/// 长度不足时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_batch_commit(payload: &[u8]) -> Result<(u32, u32)> {
    let mut cursor = Cursor::new(payload, "wal batch_commit");
    Ok((cursor.u32()?, cursor.u32()?))
}

/// 编码 `NsRegister` 负载 `[NsId][path len+bytes]`。
pub(crate) fn encode_ns_register(ns_id: u32, path: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, ns_id);
    put_bytes_u32(&mut out, path.as_bytes());
    out
}

/// 解码 `NsRegister` 负载。
///
/// # Errors
/// 长度越界或 path 非 UTF-8 时返回 [`MnemeError::Corrupted`]。
pub(crate) fn decode_ns_register(payload: &[u8]) -> Result<(u32, Arc<str>)> {
    let mut cursor = Cursor::new(payload, "wal ns_register");
    let ns_id = cursor.u32()?;
    let len = cursor.u32()? as usize;
    let path = std::str::from_utf8(cursor.take(len)?).map_err(|error| MnemeError::Corrupted {
        segment: None,
        reason: format!("wal: path 非 UTF-8:{error}"),
    })?;
    Ok((ns_id, Arc::from(path)))
}

/// 编码 `RelKindRegister` 负载 `[kind u16][name len+bytes]`。
pub(crate) fn encode_rel_kind_register(kind: u16, name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&kind.to_le_bytes());
    put_bytes_u32(&mut out, name.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::meta::json;
    use crate::core::types::{Key, NsId, RowId, SeqNo};

    fn entry() -> EntryData {
        EntryData {
            rowid: RowId::new(3),
            seqno: SeqNo::new(4),
            ns_id: NsId::new(1),
            key: Some(Key::new("k")),
            text: None,
            meta: json!({"a": 1}),
            created_at_ms: 5,
            expires_at_ms: None,
            importance: None,
            access: None,
            valid_time: None,
            confidence: None,
            provenance: None,
        }
    }

    /// 文件头往返一致。
    #[test]
    fn wal_header_roundtrip() {
        let header = encode_file_header(8, Metric::Euclidean);
        let parsed = parse_file_header(&header).expect("parse");
        assert_eq!(parsed.dimension, 8);
        assert_eq!(parsed.metric, Metric::Euclidean);
    }

    /// 多帧回放保持顺序与负载。
    #[test]
    fn wal_replay_roundtrip() {
        let header = encode_file_header(4, Metric::Cosine);
        let mut bytes = header.to_vec();
        bytes.extend_from_slice(&encode_frame(
            1,
            FrameKind::Insert,
            &encode_insert(&entry(), &[1.0, 2.0]).expect("insert"),
        ));
        bytes.extend_from_slice(&encode_frame(2, FrameKind::Delete, &encode_delete(1, "k")));
        bytes.extend_from_slice(&encode_frame(
            3,
            FrameKind::Checkpoint,
            &encode_checkpoint(2),
        ));

        let replay = replay(&bytes).expect("replay");
        assert_eq!(replay.valid_len, bytes.len());
        assert_eq!(replay.frames.len(), 3);
        assert_eq!(replay.frames[0].seqno, 1);
        assert_eq!(replay.frames[0].kind, FrameKind::Insert);
        assert_eq!(
            decode_insert(&replay.frames[0].payload).expect("insert"),
            (entry(), vec![1.0, 2.0])
        );
        assert_eq!(
            decode_delete(&replay.frames[1].payload).expect("delete"),
            (1, Arc::from("k"))
        );
        assert_eq!(decode_checkpoint(&replay.frames[2].payload).expect("cp"), 2);
    }

    /// 尾部撕裂帧被丢弃且不计入有效长度。
    #[test]
    fn wal_torn_tail_is_dropped() {
        let header = encode_file_header(4, Metric::Cosine);
        let mut bytes = header.to_vec();
        bytes.extend_from_slice(&encode_frame(
            1,
            FrameKind::Checkpoint,
            &encode_checkpoint(1),
        ));
        let good_len = bytes.len();
        bytes.extend_from_slice(&encode_frame(
            2,
            FrameKind::Checkpoint,
            &encode_checkpoint(2),
        ));
        bytes.truncate(bytes.len() - 3);

        let replay = replay(&bytes).expect("replay");
        assert_eq!(replay.valid_len, good_len);
        assert_eq!(replay.frames.len(), 1);
    }

    /// CRC 损坏的帧作为撕裂尾部被丢弃(不报错、不返回坏数据)。
    #[test]
    fn wal_bad_crc_stops_replay() {
        let header = encode_file_header(4, Metric::Cosine);
        let mut bytes = header.to_vec();
        bytes.extend_from_slice(&encode_frame(
            1,
            FrameKind::Checkpoint,
            &encode_checkpoint(1),
        ));
        let good_len = bytes.len();
        let mut frame = encode_frame(2, FrameKind::Checkpoint, &encode_checkpoint(2));
        let last = frame.len() - 1;
        frame[last] ^= 0xFF;
        bytes.extend_from_slice(&frame);

        let replay = replay(&bytes).expect("replay");
        assert_eq!(replay.valid_len, good_len);
        assert_eq!(replay.frames.len(), 1);
    }

    /// 未知帧类型 → 报错(FC-PERSIST-ERR-001)。
    #[test]
    fn wal_unknown_frame_type_errors() {
        let header = encode_file_header(4, Metric::Cosine);
        let mut bytes = header.to_vec();
        // 构造一个 CRC 正确但 type=0x7F 的帧。
        let mut covered = Vec::new();
        covered.extend_from_slice(&0_u32.to_le_bytes());
        covered.extend_from_slice(&1_u64.to_le_bytes());
        covered.push(0x7F);
        bytes.extend_from_slice(&crc32(&covered).to_le_bytes());
        bytes.extend_from_slice(&covered);
        assert!(matches!(replay(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// TouchRow / Relate / Unrelate 负载往返一致。
    #[test]
    fn wal_touch_relate_roundtrip() {
        assert_eq!(
            decode_touch_row(&encode_touch_row(7, 123, 2, 0.5)).expect("touch"),
            (7, 123, 2, 0.5)
        );
        let meta = json!({"why": "link"});
        assert_eq!(
            decode_relate(&encode_relate(1, 2, 3, 0.7, &meta)).expect("relate"),
            (1, 2, 3, 0.7, meta)
        );
        assert_eq!(
            decode_unrelate(&encode_unrelate(1, 2, 3)).expect("unrelate"),
            (1, 2, 3)
        );
    }

    /// 文件头损坏被检出。
    #[test]
    fn wal_detects_header_corruption() {
        let mut header = encode_file_header(4, Metric::Cosine);
        header[6] ^= 0xFF;
        assert!(matches!(
            parse_file_header(&header),
            Err(MnemeError::Corrupted { .. })
        ));
    }
}
