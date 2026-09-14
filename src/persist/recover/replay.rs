//! WAL 帧回放(`recover/replay.rs`,设计 04 §3.3)。

use crate::core::error::{MnemeError, Result};
use crate::memory::table::WriterState;
use crate::persist::crc32;
use crate::persist::wal::{self, FrameKind};

use super::wal_replay::{PendingFrame, apply_if_after_watermark};

/// 单帧视图(序号、类型、负载与结束偏移)。
struct FrameRef<'a> {
    seqno: u64,
    kind: FrameKind,
    payload: &'a [u8],
    end: usize,
}

/// 回放期间的批缓冲与已提交位置。
struct ReplayBuffers {
    batch: Option<Vec<(u64, FrameKind, Vec<u8>)>>,
    crc_input: Vec<u8>,
    committed_len: usize,
    watermark: u64,
}

/// `BatchCommit` 校验/应用所需的输入。
struct BatchCommitInput<'a> {
    /// 批内帧负载拼接(用于校验 `batch_crc`)。
    crc_input: &'a [u8],
    /// `BatchCommit` 帧负载(计数 + CRC)。
    payload: &'a [u8],
    /// 已物化水位。
    watermark: u64,
}

/// 回放 WAL:应用 `seqno > watermark` 的帧(设计 04 §3.3)。
///
/// 返回**已提交字节长度**(文件头 + 已提交帧);小于 `bytes.len()` 表示尾部有撕裂帧
/// 或**未闭合的批**(缺 `BatchCommit`),调用方应在可写打开时据此截断,避免残留的
/// 未提交帧在下次回放中把后续单操作事务误并入批而丢弃。
///
/// 文件短于 WAL 文件头(Checkpoint 重置中途崩溃)视为撕裂头,返回 `0` 以便可写
/// 打开时重建 WAL,绝不因半截头而拒绝打开。
///
/// # Errors
/// 文件头损坏、未知帧类型(FC-PERSIST-ERR-001)或批提交计数/CRC 不符时返回错误
/// (尾部撕裂帧由 [`wal::visit_frames`] 自行停止)。
pub(crate) fn replay_wal(
    state: &mut WriterState,
    bytes: &[u8],
    watermark: u64,
    encryption: Option<&crate::crypto::Encryption>,
) -> Result<usize> {
    if bytes.len() < wal::FILE_HEADER_LEN {
        return Ok(0);
    }
    let mut buffers = ReplayBuffers {
        batch: None,
        crc_input: Vec::new(),
        committed_len: wal::FILE_HEADER_LEN,
        watermark,
    };
    wal::visit_frames(bytes, |seqno, kind, payload, end| {
        // 加密帧(信封)先解密;未开 feature/未配置加密的库遇信封显式拒绝,
        // 绝不把密文当负载继续解析(FC-SEC-INV-028)。
        let decrypted;
        let payload = if crate::crypto::is_envelope(payload) {
            let Some(encryption) = encryption else {
                return Err(MnemeError::Unsupported { feature: "encrypt" });
            };
            decrypted = crate::crypto::open(encryption, b"wal", seqno, payload)?;
            decrypted.as_slice()
        } else {
            payload
        };
        apply_frame(
            state,
            &mut buffers,
            FrameRef {
                seqno,
                kind,
                payload,
                end,
            },
        )
    })?;
    // 未闭合的批(缺 `BatchCommit`)整体丢弃,其字节不计入已提交长度,由调用方截断。
    state.pending.clear();
    Ok(buffers.committed_len)
}

/// 应用单帧:批边界维护已提交位置,批内帧暂存,批外帧立即应用。
fn apply_frame(
    state: &mut WriterState,
    buffers: &mut ReplayBuffers,
    frame: FrameRef<'_>,
) -> Result<()> {
    match frame.kind {
        FrameKind::BatchBegin => {
            buffers.batch = Some(Vec::new());
            buffers.crc_input.clear();
        }
        FrameKind::BatchCommit => {
            commit_batch(
                state,
                &mut buffers.batch,
                BatchCommitInput {
                    crc_input: &buffers.crc_input,
                    payload: frame.payload,
                    watermark: buffers.watermark,
                },
            )?;
            // 批提交成功(或空提交)→ 该帧尾即新的已提交位置。
            buffers.committed_len = frame.end;
            buffers.crc_input.clear();
        }
        // 批内帧暂存,暂不推进已提交位置;批外帧立即应用并推进。
        _ => match buffers.batch.as_mut() {
            Some(buffered) => {
                buffered.push((frame.seqno, frame.kind, frame.payload.to_vec()));
                buffers.crc_input.extend_from_slice(frame.payload);
            }
            None => {
                apply_if_after_watermark(
                    state,
                    PendingFrame {
                        seqno: frame.seqno,
                        kind: frame.kind,
                        payload: frame.payload,
                    },
                    buffers.watermark,
                )?;
                buffers.committed_len = frame.end;
            }
        },
    }
    Ok(())
}

/// 校验 `BatchCommit` 的计数与 CRC 后,整体应用批内暂存帧。
fn commit_batch(
    state: &mut WriterState,
    batch: &mut Option<Vec<(u64, FrameKind, Vec<u8>)>>,
    input: BatchCommitInput<'_>,
) -> Result<()> {
    let (count, stored_crc) = wal::decode_batch_commit(input.payload)?;
    let Some(buffered) = batch.take() else {
        return Ok(());
    };
    if buffered.len() != count as usize || crc32(input.crc_input) != stored_crc {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "wal: 批提交计数或 CRC 不符".to_string(),
        });
    }
    for (inner_seqno, inner_kind, inner_payload) in buffered {
        apply_if_after_watermark(
            state,
            PendingFrame {
                seqno: inner_seqno,
                kind: inner_kind,
                payload: &inner_payload,
            },
            input.watermark,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::metric::Metric;

    /// 批内帧负载的 CRC(与 `Store::log` 的计算口径一致)。
    fn batch_crc(payloads: &[Vec<u8>]) -> u32 {
        let mut input = Vec::new();
        for payload in payloads {
            input.extend_from_slice(payload);
        }
        crc32(&input)
    }

    /// `BatchCommit` 的 `batch_crc` 不符 → 拒绝回放,绝不应用半批(I15)。
    #[test]
    fn replay_rejects_mismatched_batch_crc() {
        let register = wal::encode_ns_register(1, "a");
        let mut bytes = wal::encode_file_header(2, Metric::Cosine).to_vec();
        bytes.extend_from_slice(&wal::encode_frame(
            0,
            FrameKind::BatchBegin,
            &wal::encode_batch_begin(1),
        ));
        bytes.extend_from_slice(&wal::encode_frame(1, FrameKind::NsRegister, &register));
        bytes.extend_from_slice(&wal::encode_frame(
            0,
            FrameKind::BatchCommit,
            &wal::encode_batch_commit(1, batch_crc(&[register]) ^ 0xFFFF_FFFF),
        ));
        let mut state = WriterState::new();
        assert!(matches!(
            replay_wal(&mut state, &bytes, 0, None),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    /// `BatchCommit` 计数与批内帧数不符 → 拒绝回放。
    #[test]
    fn replay_rejects_mismatched_batch_count() {
        let register = wal::encode_ns_register(1, "a");
        let mut bytes = wal::encode_file_header(2, Metric::Cosine).to_vec();
        bytes.extend_from_slice(&wal::encode_frame(
            0,
            FrameKind::BatchBegin,
            &wal::encode_batch_begin(2),
        ));
        bytes.extend_from_slice(&wal::encode_frame(1, FrameKind::NsRegister, &register));
        bytes.extend_from_slice(&wal::encode_frame(
            0,
            FrameKind::BatchCommit,
            &wal::encode_batch_commit(2, batch_crc(&[register])),
        ));
        let mut state = WriterState::new();
        assert!(matches!(
            replay_wal(&mut state, &bytes, 0, None),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    /// 计数与 CRC 均正确的批被整体应用。
    #[test]
    fn replay_applies_well_formed_batch() {
        let register = wal::encode_ns_register(1, "a");
        let mut bytes = wal::encode_file_header(2, Metric::Cosine).to_vec();
        bytes.extend_from_slice(&wal::encode_frame(
            0,
            FrameKind::BatchBegin,
            &wal::encode_batch_begin(1),
        ));
        bytes.extend_from_slice(&wal::encode_frame(1, FrameKind::NsRegister, &register));
        bytes.extend_from_slice(&wal::encode_frame(
            0,
            FrameKind::BatchCommit,
            &wal::encode_batch_commit(1, batch_crc(&[register])),
        ));
        let mut state = WriterState::new();
        let committed = replay_wal(&mut state, &bytes, 0, None).expect("valid batch");
        assert_eq!(committed, bytes.len());
        assert_eq!(state.next_ns_id, 2);
    }

    /// 未闭合的批不计入已提交长度,调用方据此截断(避免吞掉后续单操作事务)。
    #[test]
    fn unclosed_batch_is_not_committed() {
        let register = wal::encode_ns_register(1, "a");
        let mut bytes = wal::encode_file_header(2, Metric::Cosine).to_vec();
        let after_header = bytes.len();
        bytes.extend_from_slice(&wal::encode_frame(
            0,
            FrameKind::BatchBegin,
            &wal::encode_batch_begin(1),
        ));
        bytes.extend_from_slice(&wal::encode_frame(1, FrameKind::NsRegister, &register));
        // 故意缺 BatchCommit。
        let mut state = WriterState::new();
        let committed = replay_wal(&mut state, &bytes, 0, None).expect("unclosed batch");
        assert_eq!(committed, after_header, "未闭合批不得计入已提交长度");
    }

    /// WAL 短于文件头(Checkpoint 重置中途崩溃)→ 视为撕裂头返回 0,不报错。
    #[test]
    fn short_wal_header_is_treated_as_torn() {
        let bytes = [b'W', b'A', b'L', b'1', 0x01];
        let mut state = WriterState::new();
        assert_eq!(
            replay_wal(&mut state, &bytes, 0, None).expect("torn header"),
            0
        );
    }
}
