//! WAL 帧编解码与回放(设计 04 §2.3、§3)。
//!
//! 文件 = 32 字节定长头 + 帧序列。每帧:
//! `[u32 crc32][u32 payload_len][u64 seqno][u8 type][payload]`,CRC 覆盖
//! `[payload_len, seqno, type, payload]`(即本帧除自身 CRC 外的全部字节)。
//!
//! 回放时若读不满 `payload_len` 或 CRC 不符 → 该帧及其后全部丢弃(只追加文件,
//! 尾部之后不可能是有效数据);未知帧类型 → **报错而非静默跳过**(FC-PERSIST-ERR-001)。
//!
//! # 子模块
//!
//! * `codec` —— 文件头 / 帧的编解码与回放遍历。
//! * `payload` —— 各帧类型的负载编解码。

use crate::core::metric::Metric;

mod codec;
mod payload;

pub(crate) use codec::*;
pub(crate) use payload::*;

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
    /// 命名空间注销(删除路径;`NsId` 作废、永不复用)。
    NsUnregister,
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

/// 一帧 WAL 记录(仅测试用;运行时回放走流式 [`visit_frames`])。
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    /// 全局单调写序号。
    pub(crate) seqno: u64,
    /// 帧类型。
    pub(crate) kind: FrameKind,
    /// 帧负载(原始字节;解释见各 `encode_*`/`decode_*`)。
    pub(crate) payload: Vec<u8>,
}

/// 回放结果:有效帧 + 有效字节长度(撕裂帧之前的长度,仅测试用)。
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Replay {
    /// 完整且 CRC 正确的帧序列。
    pub(crate) frames: Vec<Frame>,
    /// 头部 + 有效帧的总字节数;小于文件长度表示尾部有撕裂帧。
    pub(crate) valid_len: usize,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::error::MnemeError;
    use crate::core::meta::json;
    use crate::core::metric::Metric;
    use crate::core::types::{Key, NsId, RowId, SeqNo};
    use crate::persist::crc32;
    use crate::persist::msec::EntryData;

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
            &encode_insert(&entry(), &[1.0, 2.0], 42).expect("insert"),
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
            (entry(), vec![1.0, 2.0], 42)
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
        let spec = RelateSpec {
            kind: 3,
            weight: 0.7,
            meta: &meta,
        };
        assert_eq!(
            decode_relate(&encode_relate(1, 2, spec)).expect("relate"),
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
