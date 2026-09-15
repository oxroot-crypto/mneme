use std::sync::Arc;

use crate::core::error::MnemeError;
use crate::core::metric::Metric;
use crate::persist::FORMAT_VERSION;

use super::header::header_crc;
use super::*;

fn sample() -> Manifest {
    Manifest {
        dimension: 768,
        metric: Metric::Cosine,
        stopwords: true,
        next_rel_kind: 20,
        manifest_version: 42,
        watermark_seqno: 105,
        next_rowid: 7,
        next_segment_id: 3,
        next_ns_id: 2,
        namespaces: vec![NsEntry {
            ns_id: 1,
            path: Arc::from("project/session"),
        }],
        rel_kinds: vec![RelKindEntry {
            kind: 16,
            name: Arc::from("custom"),
        }],
        segments: vec![SegmentEntry {
            segment_id: 7,
            format_version: FORMAT_VERSION,
            row_count: 100,
            min_seqno: 1,
            max_seqno: 100,
            created_ms: 1_700_000_000_000,
            vsec_crc: 0xAA,
            msec_crc: 0xBB,
            hidx_crc: 0,
            entry_slot: 0,
            entry_level: 0,
        }],
    }
}

/// 全字段往返一致。
#[test]
fn manifest_roundtrip() {
    let manifest = sample();
    let bytes = encode(&manifest).expect("encode");
    assert_eq!(parse(&bytes).expect("parse"), manifest);
}

/// FC-PERSIST-POST-009(stopwords 显式开/关往返;非法值拒绝)
#[test]
fn stopwords_flag_roundtrip() {
    for stopwords in [true, false] {
        let mut manifest = sample();
        manifest.stopwords = stopwords;
        let bytes = encode(&manifest).expect("encode");
        let decoded = parse(&bytes).expect("parse");
        assert_eq!(decoded.stopwords, stopwords);
    }

    // 未知值(含 0)→ Corrupted。
    for flag in [0_u8, 9] {
        let mut bytes = encode(&sample()).expect("encode");
        bytes[17] = flag;
        let crc = header_crc(&bytes[..HEADER_LEN as usize]);
        bytes[8..12].copy_from_slice(&crc.to_le_bytes());
        assert!(
            matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })),
            "stopwords 标志 {flag} 必须拒绝"
        );
    }
}

/// 头部 CRC 损坏被检出。
#[test]
fn manifest_detects_header_corruption() {
    let mut bytes = encode(&sample()).expect("encode");
    bytes[12] ^= 0xFF;
    assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
}

/// 变长区 CRC 损坏被检出。
#[test]
fn manifest_detects_payload_corruption() {
    let mut bytes = encode(&sample()).expect("encode");
    let last = bytes.len() - 5;
    bytes[last] ^= 0xFF;
    assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
}

/// 更高/更低版本 → `UnsupportedVersion`。
#[test]
fn manifest_rejects_version_mismatch() {
    for version in [0x0100_u16, FORMAT_VERSION - 1] {
        let manifest = sample();
        let mut bytes = encode(&manifest).expect("encode");
        bytes[4..6].copy_from_slice(&version.to_le_bytes());
        let crc = header_crc(&bytes[..HEADER_LEN as usize]);
        bytes[8..12].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            parse(&bytes),
            Err(MnemeError::UnsupportedVersion { .. })
        ));
    }
}

/// FC-PERSIST-ERR-012:段号/MANIFEST 版本水位 checked——正常 +1,
/// 到 `u32::MAX`/`u64::MAX` 时返回 `IdExhausted`,绝不回绕。
#[test]
fn manifest_counters_reject_exhaustion() {
    assert_eq!(next_segment_id(41).expect("+1"), 42);
    assert!(matches!(
        next_segment_id(u32::MAX),
        Err(MnemeError::IdExhausted { kind: "segment_id" })
    ));
    assert_eq!(next_manifest_version(7).expect("+1"), 8);
    assert!(matches!(
        next_manifest_version(u64::MAX),
        Err(MnemeError::IdExhausted {
            kind: "manifest_version"
        })
    ));
}
