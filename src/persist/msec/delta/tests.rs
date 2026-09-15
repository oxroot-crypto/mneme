//! `delta` 模块个单元测试(往返排序、空区、畸形拒绝同计数上溢)。

use super::*;
use crate::core::meta::json;

fn sample() -> Vec<DeltaEntry> {
    vec![
        DeltaEntry::Unrelate {
            seqno: 9,
            tx_ms: 90,
            ns_id: 1,
            from: 3,
            to: 4,
            kind: 0,
        },
        DeltaEntry::Access {
            seqno: 5,
            tx_ms: 50,
            ns_id: 1,
            rowid: 3,
            last_access_ms: 55,
            access_delta: 2,
            importance_delta: 0.0,
        },
        DeltaEntry::Relate {
            seqno: 6,
            tx_ms: 60,
            ns_id: 2,
            from: 1,
            to: 3,
            kind: 7,
            weight: 0.25,
            meta: json!({"why": "test"}),
        },
    ]
}

/// FC-PERSIST-POST-010(往返一致 + 按 `(target, seqno, kind)` 排序)
#[test]
fn delta_roundtrip_is_sorted_and_lossless() {
    let bytes = encode_delta(&sample()).expect("encode");
    let decoded = decode_delta(&bytes).expect("decode");
    // 排序键 (target, seqno, kind):(1,6) Relate、(3,5) Access、(3,9) Unrelate。
    assert_eq!(decoded.len(), 3);
    assert!(matches!(decoded[0], DeltaEntry::Relate { from: 1, .. }));
    assert!(matches!(
        decoded[1],
        DeltaEntry::Access {
            rowid: 3,
            access_delta: 2,
            ..
        }
    ));
    assert!(matches!(decoded[2], DeltaEntry::Unrelate { from: 3, .. }));
    let reencoded = encode_delta(&decoded).expect("reencode");
    assert_eq!(bytes, reencoded, "解码后重编码必须逐字节一致");
}

/// FC-PERSIST-POST-010(空区 = 无跨段变更)
#[test]
fn empty_delta_is_valid() {
    assert!(decode_delta(&[]).expect("空区").is_empty());
    assert!(encode_delta(&[]).expect("空编码").is_empty());
}

/// FC-PERSIST-ERR-011(条目数超 `u16::MAX` → `LimitExceeded`,绝不静默截断计数)
#[test]
fn delta_count_overflow_is_rejected() {
    let entry = |rowid: u64| DeltaEntry::Access {
        seqno: 1,
        tx_ms: 1,
        ns_id: 0,
        rowid,
        last_access_ms: 1,
        access_delta: 1,
        importance_delta: 0.0,
    };
    let boundary: Vec<DeltaEntry> = (0..u16::MAX as u64).map(entry).collect();
    let bytes = encode_delta(&boundary).expect("u16::MAX 条必须可编码");
    assert_eq!(
        decode_delta(&bytes).expect("decode").len(),
        u16::MAX as usize
    );

    let overflow: Vec<DeltaEntry> = (0..u16::MAX as u64 + 1).map(entry).collect();
    assert!(matches!(
        encode_delta(&overflow),
        Err(MnemeError::LimitExceeded { .. })
    ));
}

/// FC-PERSIST-ERR-011(魔数/CRC/未知 kind/尾部残留/截断/长度越界 → Corrupted)
#[test]
fn malformed_delta_is_rejected() {
    rejects_magic_crc_and_truncation();
    rejects_unknown_kind_and_residual_tail();
    rejects_oversized_meta_and_arbitrary_bytes();
}

/// 魔数、尾部 CRC 与截断输入必须拒绝。
fn rejects_magic_crc_and_truncation() {
    let bytes = encode_delta(&sample()).expect("encode");
    let mut bad_magic = bytes.clone();
    bad_magic[0] = b'X';
    assert!(matches!(
        decode_delta(&bad_magic),
        Err(MnemeError::Corrupted { .. })
    ));

    let mut bad_crc = bytes.clone();
    let last = bad_crc.len() - 1;
    bad_crc[last] ^= 0xFF;
    assert!(matches!(
        decode_delta(&bad_crc),
        Err(MnemeError::Corrupted { .. })
    ));

    let mut truncated = bytes;
    truncated.truncate(HEADER_LEN + 1);
    assert!(matches!(
        decode_delta(&truncated),
        Err(MnemeError::Corrupted { .. })
    ));
}

/// 未知 kind 与条目区尾部残留必须拒绝(重算 CRC 后仍按结构校验拦下)。
fn rejects_unknown_kind_and_residual_tail() {
    let bytes = encode_delta(&sample()).expect("encode");
    // 未知 kind:改写第一条公共前缀后重算 CRC。
    let mut unknown = bytes.clone();
    unknown[HEADER_LEN] = 3;
    let crc = crc32(&unknown[HEADER_LEN..]);
    unknown[8..12].copy_from_slice(&crc.to_le_bytes());
    assert!(matches!(
        decode_delta(&unknown),
        Err(MnemeError::Corrupted { .. })
    ));

    // 尾部残留:在合法条目区后追加一个字节并重算 CRC。
    let mut residual = bytes;
    residual.push(0xAB);
    let crc = crc32(&residual[HEADER_LEN..]);
    residual[8..12].copy_from_slice(&crc.to_le_bytes());
    assert!(matches!(
        decode_delta(&residual),
        Err(MnemeError::Corrupted { .. })
    ));
}

/// 长度字段越界必须拒绝;任意前缀字节不得 panic。
fn rejects_oversized_meta_and_arbitrary_bytes() {
    // 长度越界:Relate 的 meta 长度字段改成超大值(挑 kinds 里的 Relate 条目)。
    let relate_bytes = encode_delta(&[sample().remove(2)]).expect("单条 Relate");
    let mut oversized = relate_bytes.clone();
    let meta_len_offset = HEADER_LEN + COMMON_BYTES + 8 + 8 + 2 + 4;
    oversized[meta_len_offset..meta_len_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let crc = crc32(&oversized[HEADER_LEN..]);
    oversized[8..12].copy_from_slice(&crc.to_le_bytes());
    assert!(matches!(
        decode_delta(&oversized),
        Err(MnemeError::Corrupted { .. })
    ));

    // 任意字节不 panic。
    let bytes = encode_delta(&sample()).expect("encode");
    for len in 0..bytes.len() {
        let _ = decode_delta(&bytes[..len]);
    }
}
