//! `vsec` 编解码个单元测试:往返、损坏检出、量化副本同 feature 门控。

use super::*;

fn sample(count: usize) -> (Vec<Vec<f32>>, Vec<f32>, Vec<bool>) {
    let vectors: Vec<Vec<f32>> = (0..count)
        .map(|row| (0..4).map(|col| (row * 4 + col) as f32 * 0.5).collect())
        .collect();
    let norms = vectors
        .iter()
        .map(|v| v.iter().map(|x| x * x).sum())
        .collect();
    let dead = (0..count).map(|row| row % 2 == 1).collect();
    (vectors, norms, dead)
}

fn encode_sample(count: usize) -> Vec<u8> {
    let (vectors, norms, dead) = sample(count);
    let refs: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
    encode(&VsecInput {
        dimension: 4,
        metric: Metric::Cosine,
        created_unix_ms: 1_700_000_000_000,
        vectors: &refs,
        norms: &norms,
        dead: &dead,
        quant: VectorFormat::F32,
        quant_params: &[],
        quant_codes: &[],
    })
    .expect("encode")
}

/// i8 qvec 样本:参数表按列统计构造,码流逐行编码。
fn encode_i8_sample(count: usize) -> Vec<u8> {
    let (vectors, norms, dead) = sample(count);
    let refs: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
    let params = crate::quant::scalar_i8::build_params(&refs, 4).expect("build_params");
    let codes: Vec<Vec<u8>> = vectors
        .iter()
        .map(|vector| crate::quant::scalar_i8::encode_row(vector, &params))
        .collect();
    let code_refs: Vec<&[u8]> = codes.iter().map(Vec::as_slice).collect();
    encode(&VsecInput {
        dimension: 4,
        metric: Metric::Cosine,
        created_unix_ms: 1_700_000_000_000,
        vectors: &refs,
        norms: &norms,
        dead: &dead,
        quant: VectorFormat::I8Rescored,
        quant_params: &params.table(),
        quant_codes: &code_refs,
    })
    .expect("encode")
}

/// 往返:头部字段、向量、范数与删除位图一致。
#[test]
fn vsec_roundtrip() {
    let bytes = encode_sample(3);
    let mut view = parse(&bytes).expect("parse");
    assert_eq!(view.row_count(), 3);
    assert_eq!(view.quant(), VectorFormat::F32);
    assert_eq!(view.header().dimension, 4);
    assert_eq!(view.header().metric, Metric::Cosine);
    assert_eq!(view.header().created_unix_ms, 1_700_000_000_000);
    assert_eq!(view.vector(0).expect("row0"), vec![0.0, 0.5, 1.0, 1.5]);
    assert_eq!(view.vector(2).expect("row2"), vec![4.0, 4.5, 5.0, 5.5]);
    assert!(!view.is_dead(0));
    assert!(view.is_dead(1));
    assert!(!view.is_dead(2));
    view.verify_payload().expect("payload crc");
    assert!(view.vector(3).is_none());
}

/// FC-QUANT-POST-002:i8 qvec 往返(参数表逐位一致、码流逐行一致)。
#[test]
fn vsec_i8_quantized_roundtrip() {
    let bytes = encode_i8_sample(3);
    let mut view = parse(&bytes).expect("parse");
    assert_eq!(view.quant(), VectorFormat::I8Rescored);
    let (vectors, _, _) = sample(3);
    let refs: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
    let params = crate::quant::scalar_i8::build_params(&refs, 4).expect("build_params");
    assert_eq!(view.quant_params(), params.table());
    for (row, vector) in vectors.iter().enumerate() {
        let expected = crate::quant::scalar_i8::encode_row(vector, &params);
        assert_eq!(view.quant_row(row).expect("quant row"), expected.as_slice());
    }
    assert!(view.quant_row(3).is_none());
    view.verify_payload().expect("payload crc");
}

/// FC-QUANT-ERR-003:未知 quant 编码 → `Corrupted`。
#[test]
fn vsec_rejects_unknown_quant_code() {
    let mut bytes = encode_sample(1);
    bytes[13] = 9;
    let crc = crc32(&bytes[0..32]);
    bytes[32..36].copy_from_slice(&crc.to_le_bytes());
    assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
}

/// FC-QUANT-ERR-003:i8 参数表非有限值 → `Corrupted`。
#[test]
fn vsec_rejects_malformed_i8_params() {
    let mut bytes = encode_i8_sample(2);
    // 参数表起始 = 64(头) + 2 行 × 32B(vec 区) + 2 行 × 4B(norm 区)。
    let params_start = 64 + 2 * 32 + 2 * 4;
    bytes[params_start..params_start + 4].copy_from_slice(&f32::NAN.to_le_bytes());
    let payload_crc = crc32(&bytes[64..bytes.len() - 4]);
    let tail = bytes.len() - 4;
    bytes[tail..].copy_from_slice(&payload_crc.to_le_bytes());
    assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
}

/// FC-QUANT-ERR-003:文件长度与头部布局不符(截断/多余字节)→ `Corrupted`,
/// 绝不按短读部分解析。
#[test]
fn vsec_rejects_length_mismatch() {
    let bytes = encode_sample(2);
    assert!(matches!(
        parse(&bytes[..bytes.len() - 1]),
        Err(MnemeError::Corrupted { .. })
    ));
    let mut longer = bytes;
    longer.push(0);
    assert!(matches!(parse(&longer), Err(MnemeError::Corrupted { .. })));
}

/// 跨块(> 1024 行)删除位图仍按位正确。
#[test]
fn vsec_bitmap_spans_blocks() {
    let bytes = encode_sample(1025);
    let view = parse(&bytes).expect("parse");
    // 行 1023(奇数)不可见、行 1024(偶数)可见,跨块边界仍按位正确。
    assert!(view.is_dead(1023));
    assert!(!view.is_dead(1024));
    assert_eq!(view.row_count(), 1025);
}

/// 头部 CRC 被翻转后必须被检出。
#[test]
fn vsec_detects_header_corruption() {
    let mut bytes = encode_sample(1);
    bytes[8] ^= 0xFF;
    assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
}

/// payload CRC 被翻转后必须被检出。
#[test]
fn vsec_detects_payload_corruption() {
    let mut bytes = encode_sample(1);
    let last = bytes.len() - 5;
    bytes[last] ^= 0x01;
    let mut view = parse(&bytes).expect("parse");
    assert!(matches!(
        view.verify_payload(),
        Err(MnemeError::Corrupted { .. })
    ));
}

/// 魔数不符 → `Corrupted`。
#[test]
fn vsec_rejects_bad_magic() {
    let mut bytes = encode_sample(1);
    bytes[0] = b'X';
    assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
}

/// 更高/更低版本 → `UnsupportedVersion`(I18)。
#[test]
fn vsec_rejects_version_mismatch() {
    // 高/低版本都必须拒绝(精确匹配,无旧格式兼容)。
    for version in [0x0100_u16, crate::persist::FORMAT_VERSION - 1] {
        let mut bytes = encode_sample(1);
        bytes[4..6].copy_from_slice(&version.to_le_bytes());
        // 重新计算头部 CRC,使版本成为唯一错误来源。
        let crc = crc32(&bytes[0..32]);
        bytes[32..36].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            parse(&bytes),
            Err(MnemeError::UnsupportedVersion { .. })
        ));
    }
}

/// f16 副本(仅 feature `quant-f16`)往返;关闭 feature 时编码被拒绝。
#[cfg(feature = "quant-f16")]
#[test]
fn vsec_f16_quantized_roundtrip() {
    let (vectors, norms, dead) = sample(2);
    let refs: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
    let codes: Vec<Vec<u8>> = vectors
        .iter()
        .map(|vector| crate::quant::f16::encode_row(vector))
        .collect();
    let code_refs: Vec<&[u8]> = codes.iter().map(Vec::as_slice).collect();
    let bytes = encode(&VsecInput {
        dimension: 4,
        metric: Metric::Cosine,
        created_unix_ms: 0,
        vectors: &refs,
        norms: &norms,
        dead: &dead,
        quant: VectorFormat::F16,
        quant_params: &[],
        quant_codes: &code_refs,
    })
    .expect("encode");
    let view = parse(&bytes).expect("parse");
    assert_eq!(view.quant(), VectorFormat::F16);
    assert_eq!(view.quant_row(0).expect("row0"), codes[0].as_slice());
    assert_eq!(view.quant_row(1).expect("row1"), codes[1].as_slice());
    assert!(view.quant_params().is_empty());
}

/// FC-QUANT-ERR-001:未开 feature 时编码 f16 副本必须报 `Unsupported`。
#[cfg(not(feature = "quant-f16"))]
#[test]
fn vsec_f16_encode_requires_feature() {
    let (vectors, norms, dead) = sample(1);
    let refs: Vec<&[f32]> = vectors.iter().map(Vec::as_slice).collect();
    let row = [0_u8; 8];
    let codes: [&[u8]; 1] = [&row];
    let error = encode(&VsecInput {
        dimension: 4,
        metric: Metric::Cosine,
        created_unix_ms: 0,
        vectors: &refs,
        norms: &norms,
        dead: &dead,
        quant: VectorFormat::F16,
        quant_params: &[],
        quant_codes: &codes,
    })
    .expect_err("f16 编码应被 feature 门控拒绝");
    assert!(matches!(
        error,
        MnemeError::Unsupported {
            feature: "quant-f16"
        }
    ));
}
