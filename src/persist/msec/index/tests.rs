//! `index` 轻量索引区编解码个单元测试。

use super::super::inverted::decode_inverted;
use super::*;
use crate::core::meta::json;
use crate::core::types::{Key, NsId, RowId, SeqNo};
use crate::memory::table::SlotData;
use proptest::prelude::*;

fn slot_data(index: usize, meta: crate::core::meta::Meta) -> SlotData {
    SlotData {
        rowid: RowId::new(index as u64),
        ns_id: NsId::new(1),
        ns_path: Arc::from("n"),
        seqno: SeqNo::new(index as u64 + 1),
        key: Some(Key::new(format!("k{index}"))),
        vector: crate::memory::lazy::VectorStorage::owned(Arc::from(
            vec![0.0_f32].into_boxed_slice(),
        )),
        norm_sq: 0.0,
        text: None,
        text_hash: None,
        meta,
        created_at: 1_000,
        expires_at: None,
        importance: 0.5,
        confidence: 1.0,
        valid_from: 1_000,
        valid_to: None,
        provenance: None,
        tx_ms: 1_000,
        deleted: false,
    }
}

/// FC-PERSIST-POST-008(字段字典往返)
#[test]
fn field_dict_roundtrip() {
    let fields = vec![
        (Arc::from("created_at"), FieldKind::Ts),
        (Arc::from("rank"), FieldKind::Num),
        (Arc::from("key"), FieldKind::Str),
    ];
    let bytes = encode_field_dict(&fields);
    let decoded = decode_field_dict(&bytes).expect("decode");
    assert_eq!(decoded.len(), 3);
    assert_eq!(decoded[1].name.as_ref(), "rank");
    assert_eq!(decoded[1].kind, FieldKind::Num);
    assert_eq!(decoded[2].kind, FieldKind::Str);
}

/// FC-PERSIST-POST-008(zone map 往返与结构校验)
#[test]
fn zmap_roundtrip_and_validation() {
    let fields = vec![
        (Arc::from("created_at"), FieldKind::Ts),
        (Arc::from("rank"), FieldKind::Num),
        (Arc::from("key"), FieldKind::Str),
    ];
    let mut zones = ZoneIndex::new(16);
    zones.observe(0, &slot_data(0, json!({"rank": 7})));
    zones.observe(1, &slot_data(1, json!({"rank": 3})));
    let mut bytes = encode_zmap(&zones, &fields, 1);
    bytes.extend_from_slice(&encode_ttl_map(&[i64::MAX]));
    let defs = decode_field_dict(&encode_field_dict(&fields)).expect("defs");
    validate_zmap(&bytes, &defs, 1).expect("valid");
    // 截断与块数不符必须被检出。
    assert!(validate_zmap(&bytes[..bytes.len() - 1], &defs, 1).is_err());
    assert!(validate_zmap(&bytes, &defs, 2).is_err());
}

/// FC-LIFE-CPLX-001(`ttl_map` 往返;尾部长度必须恰为 `block_count × 8`)
#[test]
fn ttl_map_roundtrip_and_invalid_tail() {
    let fields = vec![
        (Arc::from("created_at"), FieldKind::Ts),
        (Arc::from("key"), FieldKind::Str),
    ];
    let zones = ZoneIndex::new(16);
    let mut with_ttl = encode_zmap(&zones, &fields, 2);
    let defs = decode_field_dict(&encode_field_dict(&fields)).expect("defs");
    with_ttl.extend_from_slice(&encode_ttl_map(&[1_700_000_000_000, i64::MAX]));
    let decoded = decode_ttl_map(&with_ttl, &defs, 2).expect("decode");
    assert_eq!(decoded, vec![1_700_000_000_000, i64::MAX]);
    validate_zmap(&with_ttl, &defs, 2).expect("valid");

    // 缺尾 / 尾长不符 → Corrupted。
    let missing = encode_zmap(&zones, &fields, 2);
    assert!(matches!(
        decode_ttl_map(&missing, &defs, 2),
        Err(MnemeError::Corrupted { .. })
    ));
    assert!(validate_zmap(&missing, &defs, 2).is_err());
    let mut bad = with_ttl.clone();
    bad.push(0);
    assert!(matches!(
        decode_ttl_map(&bad, &defs, 2),
        Err(MnemeError::Corrupted { .. })
    ));
    assert!(validate_zmap(&bad, &defs, 2).is_err());
}

/// FC-PERSIST-POST-008(bloom 往返;否定语义保持)
#[test]
fn bloom_roundtrip() {
    let mut bloom = BloomSet::new(64, 0.01);
    bloom.insert("alpha");
    bloom.insert("beta");
    let bytes = encode_bloom(&bloom, 7);
    let decoded = decode_bloom(&bytes).expect("decode");
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].0, 7);
    assert!(decoded[0].1.maybe_contains("alpha"));
    assert!(decoded[0].1.maybe_contains("beta"));
    assert!(!decoded[0].1.maybe_contains("definitely-absent"));
}

/// FC-PERSIST-ERR-010(畸形区结构 → Corrupted,不 panic)
#[test]
fn malformed_regions_are_rejected() {
    assert!(decode_field_dict(&[0xFF, 0xFF, 0xFF, 0xFF]).is_err());
    assert!(decode_bloom(&[]).is_err());
    assert!(decode_inverted(&[0xFF; 4], &[]).is_err());
    // 字段类别未知。
    let mut dict = Vec::new();
    put_u32(&mut dict, 1);
    put_u16(&mut dict, 0);
    dict.push(9);
    put_bytes_u32(&mut dict, b"x");
    assert!(decode_field_dict(&dict).is_err());

    // 字段字典尾部残留:合法条目后多 1 字节必须拒绝。
    let mut dict = encode_field_dict(&[(Arc::from("x"), FieldKind::Num)]);
    assert!(decode_field_dict(&dict).is_ok());
    dict.push(0);
    assert!(decode_field_dict(&dict).is_err());

    // zone map 尾部残留:合法区字节后多 1 字节必须拒绝。
    let fields = vec![(Arc::from("created_at"), FieldKind::Ts)];
    let defs = decode_field_dict(&encode_field_dict(&fields)).expect("defs");
    let mut zmap = encode_zmap(&ZoneIndex::new(16), &fields, 1);
    zmap.extend_from_slice(&encode_ttl_map(&[i64::MAX]));
    assert!(validate_zmap(&zmap, &defs, 1).is_ok());
    zmap.push(0);
    assert!(validate_zmap(&zmap, &defs, 1).is_err());

    // bloom:零位长 / 非 64 倍数 / 哈希位置数越界([1,64] 之外)必须拒绝。
    let bloom_bytes = |bit_len: u32, k: u32| {
        let mut bytes = Vec::new();
        put_u32(&mut bytes, 1);
        put_u16(&mut bytes, 0);
        put_u32(&mut bytes, bit_len);
        put_u32(&mut bytes, k);
        for _ in 0..bit_len.div_ceil(64) {
            put_u64(&mut bytes, 0);
        }
        bytes
    };
    assert!(decode_bloom(&bloom_bytes(0, 7)).is_err());
    assert!(decode_bloom(&bloom_bytes(65, 7)).is_err());
    assert!(decode_bloom(&bloom_bytes(64, 0)).is_err());
    assert!(decode_bloom(&bloom_bytes(64, 65)).is_err());
    // 合法下界对照:bit_len=64、k=64 必须可解码。
    assert!(decode_bloom(&bloom_bytes(64, 64)).is_ok());
}

/// FC-PERSIST-ERR-010(任意字节不 panic)
#[test]
fn decode_regions_never_panics_on_arbitrary_bytes() {
    proptest!(|(bytes in prop::collection::vec(any::<u8>(), 0..256))| {
        // 只证不 panic:返回 Ok/Err 均允许,不对结构合法性作断言。
        let _ = decode_field_dict(&bytes);
        let _ = decode_bloom(&bytes);
        let _ = validate_zmap(&bytes, &[], 0);
    });
}
