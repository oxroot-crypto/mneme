//! `inverted` 模块个单元测试(往返、重排排序、畸形拒绝同任意字节不 panic)。

use super::*;
use proptest::prelude::*;

/// FC-PERSIST-POST-008(倒排往返:经重排映射还原为全局槽位)
#[test]
fn inverted_roundtrip_with_remap() {
    let mut index = InvertedIndex::default();
    let ns = NsId::new(1);
    index.insert_text(SlotId::new(0), ns, "alpha beta", false);
    index.insert_text(SlotId::new(1), ns, "beta gamma delta", false);
    let bytes = encode_inverted(&index).expect("encode");
    // 段内槽位 0/1 重排为全局槽位 1/0。
    let remap = [1_u32, 0_u32];
    let decoded = decode_inverted(&bytes, &remap).expect("decode");
    let beta = decoded.postings_of(ns, "beta").expect("beta");
    assert_eq!(beta.len(), 2);
    assert_eq!(beta[0].slot, SlotId::new(0));
    assert_eq!(beta[1].slot, SlotId::new(1));
    let docs: std::collections::HashMap<SlotId, u32> = decoded
        .docs_of(ns)
        .map(|(slot, doc_len)| (*slot, *doc_len))
        .collect();
    assert_eq!(docs.get(&SlotId::new(0)), Some(&3));
    assert_eq!(docs.get(&SlotId::new(1)), Some(&2));
}

#[test]
fn postings_are_sorted_after_remap() {
    let postings = decode_postings(
        &{
            let mut bytes = Vec::new();
            varint::encode_u32(0, &mut bytes);
            varint::encode_u32(2, &mut bytes);
            varint::encode_u32(1, &mut bytes);
            varint::encode_u32(3, &mut bytes);
            bytes
        },
        2,
        &[5, 3],
    )
    .expect("decode");
    assert_eq!(postings[0].slot, SlotId::new(3));
    assert_eq!(postings[0].tf, 3);
    assert_eq!(postings[1].slot, SlotId::new(5));
    assert_eq!(postings[1].tf, 2);
}

/// FC-PERSIST-ERR-010(畸形倒排区 → `Corrupted`,不 panic)
#[test]
fn malformed_inverted_is_rejected_without_panic() {
    rejects_out_of_range_offsets();
    rejects_tf_overflow();
    rejects_tail_duplicates_and_zero_doc_len();
}

/// 声明越界的 offset/length 必须拒绝而非回绕。
fn rejects_out_of_range_offsets() {
    assert!(decode_inverted(&[0xFF; 4], &[]).is_err());
    let mut bytes = Vec::new();
    put_u32(&mut bytes, 1);
    put_u32(&mut bytes, 1);
    put_bytes_u32(&mut bytes, b"t");
    put_u32(&mut bytes, 1);
    put_u64(&mut bytes, u64::MAX);
    put_u32(&mut bytes, u32::MAX);
    put_u64(&mut bytes, 0);
    assert!(matches!(
        decode_inverted(&bytes, &[0]),
        Err(MnemeError::Corrupted { .. })
    ));
}

/// 词频合并溢出必须报错。
fn rejects_tf_overflow() {
    let mut postings = Vec::new();
    varint::encode_u32(0, &mut postings);
    varint::encode_u32(u32::MAX, &mut postings);
    varint::encode_u32(0, &mut postings);
    varint::encode_u32(u32::MAX, &mut postings);
    assert!(decode_postings(&postings, 2, &[7]).is_err());
}

/// 区尾残留、词条重复与 `doc_len = 0` 必须拒绝。
fn rejects_tail_duplicates_and_zero_doc_len() {
    // 区尾残留:合法编码后多 1 字节必须拒绝。
    let mut index = InvertedIndex::default();
    index.insert_text(SlotId::new(0), NsId::new(1), "alpha", false);
    let mut bytes = encode_inverted(&index).expect("encode");
    assert!(decode_inverted(&bytes, &[0]).is_ok());
    bytes.push(0);
    assert!(matches!(
        decode_inverted(&bytes, &[0]),
        Err(MnemeError::Corrupted { .. })
    ));

    // 词条重复:同 `(ns_id, term)` 出现两次必须拒绝。
    let mut bytes = Vec::new();
    put_u32(&mut bytes, 2);
    for _ in 0..2 {
        put_u32(&mut bytes, 1);
        put_bytes_u32(&mut bytes, b"t");
        put_u32(&mut bytes, 0);
        put_u64(&mut bytes, 0);
        put_u32(&mut bytes, 0);
    }
    put_u64(&mut bytes, 0);
    put_u32(&mut bytes, 0);
    assert!(matches!(
        decode_inverted(&bytes, &[]),
        Err(MnemeError::Corrupted { .. })
    ));

    // doc 区 `doc_len = 0`:会使 BM25 `avgdl = 0` 产生 NaN 分数,必须拒绝。
    let mut bytes = Vec::new();
    put_u32(&mut bytes, 0);
    put_u64(&mut bytes, 0);
    put_u32(&mut bytes, 1);
    put_u32(&mut bytes, 1);
    put_u32(&mut bytes, 0);
    put_u32(&mut bytes, 0);
    assert!(matches!(
        decode_inverted(&bytes, &[0]),
        Err(MnemeError::Corrupted { .. })
    ));
}

/// FC-PERSIST-ERR-010(任意字节 + 任意 remap 不 panic)
#[test]
fn decode_inverted_never_panics_on_arbitrary_bytes() {
    proptest!(|(
        bytes in prop::collection::vec(any::<u8>(), 0..256),
        remap in prop::collection::vec(any::<u32>(), 0..16),
    )| {
        // 只证不 panic:decode 成败与结构合法性均不作断言。
        let _ = decode_inverted(&bytes, &remap);
    });
}
