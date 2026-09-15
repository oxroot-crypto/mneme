//! `state` 段重建同写状态初始化个单元测试。

use super::*;
use crate::core::error::MnemeError;
use crate::core::options::VectorFormat;
use crate::persist::msec::VersionRow;

/// 构造只带 relations 区的空 msec 段(无槽位)。
fn msec_only_segment(relations: &[u8]) -> Vec<u8> {
    msec::encode(&msec::MsecInput {
        slots: &[],
        ns_stats: &[],
        delta: &[],
        relations,
        field_dict: &[],
        zmap: &[],
        bloom: &[],
        inverted: &[],
        compression: crate::core::options::Compression::None,
    })
    .expect("encode")
}

/// 构造一个空的合法段字节(用于恢复顺序测试)。
fn empty_segment_bytes(id: u32) -> SegmentBytes {
    let vsec = vsec::encode(&vsec::VsecInput {
        dimension: 2,
        metric: crate::core::metric::Metric::Cosine,
        created_unix_ms: 0,
        vectors: &[],
        norms: &[],
        dead: &[],
        quant: VectorFormat::F32,
        quant_params: &[],
        quant_codes: &[],
    })
    .expect("vsec");
    let empty_edges = crate::persist::edges::encode(&[], false, false).expect("edges");
    SegmentBytes {
        segment_id: id,
        vsec: ByteFile::from_bytes(id, vsec),
        msec: ByteFile::from_bytes(id, msec_only_segment(&empty_edges)),
        hidx: None,
    }
}

/// FC-MODEL-POST-007:无 `FLAG_FULL` 的增量段按 upsert 应用(空关系表)
/// 不得清掉先前段建立的边。
#[test]
fn incremental_relations_are_upserted() {
    let edge = crate::persist::edges::EdgeData {
        from: 7,
        to: 9,
        kind: 1,
        weight: 0.5,
        meta: crate::core::meta::Meta::Null,
    };
    let with_edge = crate::persist::edges::encode(&[edge], false, false).expect("encode");
    let empty = crate::persist::edges::encode(&[], false, false).expect("encode");

    let mut state = WriterState::new();
    let seg0 = msec_only_segment(&with_edge);
    let view0 = msec::parse(&seg0).expect("parse seg0");
    apply_relations(&mut state, &view0).expect("apply seg0");
    assert_eq!(
        state
            .out_edges
            .get(&crate::core::types::RowId::new(7))
            .map(Vec::len),
        Some(1)
    );

    // 增量段(空关系表、无 FULL 位):upsert 不得清掉先前个边。
    let seg1 = msec_only_segment(&empty);
    let view1 = msec::parse(&seg1).expect("parse seg1");
    apply_relations(&mut state, &view1).expect("apply seg1");
    assert_eq!(
        state
            .out_edges
            .get(&crate::core::types::RowId::new(7))
            .map(Vec::len),
        Some(1),
        "增量段 upsert 不得清掉先前段个边"
    );
}

/// FC-PERSIST-POST-012:恢复按段号升序回放(防御 MANIFEST 乱序/手工修复)。
#[test]
fn collect_versions_orders_segments_by_id() {
    let segments = [empty_segment_bytes(1), empty_segment_bytes(0)];
    let collected = collect_versions(&segments, false, false).expect("collect");
    assert_eq!(collected.parsed_ids, vec![0, 1], "段必须按编号升序回放");
}

/// FC-PERSIST-ERR-010(新区段四区结构畸形且 fail-fast → `Corrupted`)
#[test]
fn malformed_region_section_is_error() {
    // 非空 field_dict + 空 zmap:decode 阶段必然失败,调用方 fail-fast 时应上报。
    let fields = msec::encode_field_dict(&[(Arc::from("key"), msec::FieldKind::Str)]);
    let dict_offset = crate::persist::msec::HEADER_LEN as u64;
    let mut bytes = vec![0_u8; crate::persist::msec::HEADER_LEN as usize];
    bytes[0..4].copy_from_slice(b"MSC1");
    bytes[4..6].copy_from_slice(&crate::persist::FORMAT_VERSION.to_le_bytes());
    bytes[6..8].copy_from_slice(&crate::persist::msec::HEADER_LEN.to_le_bytes());
    // field_dict 指向紧随头部的数据区。
    bytes[16..24].copy_from_slice(&dict_offset.to_le_bytes());
    bytes[24..32].copy_from_slice(&(fields.len() as u64).to_le_bytes());
    let crc = crate::persist::crc32(&bytes[0..160]);
    bytes[160..164].copy_from_slice(&crc.to_le_bytes());
    bytes.extend_from_slice(&fields);
    bytes.extend_from_slice(&crate::persist::crc32(&fields).to_le_bytes());

    let view = msec::parse(&bytes).expect("段头本身合法");
    assert!(!view.field_dict_bytes().is_empty());
    let mut state = WriterState::new();
    let result = load_disk_indexes(&mut state, &view, &[]);
    assert!(
        matches!(result, Err(MnemeError::Corrupted { .. })),
        "畸形区结构必须报 Corrupted"
    );
}

/// FC-PERSIST-ERR-010(字段字典缺失或其它索引区畸形 → `Corrupted`)
#[test]
fn half_indexed_section_is_rejected() {
    let zmap = [0_u8; 8];
    let zmap_offset = crate::persist::msec::HEADER_LEN as u64;
    let mut bytes = vec![0_u8; crate::persist::msec::HEADER_LEN as usize];
    bytes[0..4].copy_from_slice(b"MSC1");
    bytes[4..6].copy_from_slice(&crate::persist::FORMAT_VERSION.to_le_bytes());
    bytes[6..8].copy_from_slice(&crate::persist::msec::HEADER_LEN.to_le_bytes());
    // zmap 的 offset/len 对在头部偏移 96/104(见 `encode_header`)。
    bytes[96..104].copy_from_slice(&zmap_offset.to_le_bytes());
    bytes[104..112].copy_from_slice(&(zmap.len() as u64).to_le_bytes());
    let crc = crate::persist::crc32(&bytes[0..160]);
    bytes[160..164].copy_from_slice(&crc.to_le_bytes());
    bytes.extend_from_slice(&zmap);
    bytes.extend_from_slice(&crate::persist::crc32(&zmap).to_le_bytes());

    let view = msec::parse(&bytes).expect("段头本身合法");
    assert!(view.field_dict_bytes().is_empty());
    let mut state = WriterState::new();
    assert!(
        matches!(
            load_disk_indexes(&mut state, &view, &[]),
            Err(MnemeError::Corrupted { .. })
        ),
        "字段字典缺失或索引区畸形必须按损坏拒绝,不得静默跳过校验"
    );
}

/// 构造一条版本行(仅 `rowid`/`slot_id` 对本模块测试有意义)。
fn version(rowid: u64, slot_id: u32) -> VersionRow {
    VersionRow {
        rowid,
        seqno: rowid,
        tx_ms: 0,
        slot_id,
        doc_offset: 0,
    }
}

/// FC-PERSIST-ERR-009:映射按"(rowid, seqno) 有序链位置"重排(非恒等);
/// 多段各自独立映射,互不影响。
#[test]
fn build_remaps_maps_slots_in_version_chain_order() {
    // 段内槽位 1 的版本排在链首、槽位 0 排在其后 → 映射必须非恒等。
    let versions = [(version(1, 1), 0), (version(2, 0), 0)];
    let remaps = build_remaps(&versions, &[2], &[0]).expect("合法布局不得报错");
    assert_eq!(remaps.len(), 1);
    assert_eq!(remaps[0].remap, vec![1, 0], "remap[段内槽位] = 有序链位置");

    // 多段:段 7 占全局槽位 0/1,段 8 的版本排在其后。
    let multi_versions = [(version(1, 1), 0), (version(2, 0), 0), (version(3, 0), 1)];
    let multi = build_remaps(&multi_versions, &[2, 1], &[7, 8]).expect("多段独立映射");
    assert_eq!(multi[0].segment_id, 7);
    assert_eq!(multi[0].remap, vec![1, 0]);
    assert_eq!(multi[1].segment_id, 8);
    assert_eq!(multi[1].remap, vec![2]);
}

/// FC-PERSIST-ERR-009:槽位越界、重复、存在未被版本行引用的槽位
/// (vsec/msec 行数不一致)→ `Corrupted`,绝不静默映射到槽位 0。
#[test]
fn build_remaps_rejects_out_of_range_duplicate_or_unreferenced_slots() {
    // 槽位越界:slot 2 不在 [0, row_count)。
    let out_of_range = [(version(1, 2), 0)];
    assert!(matches!(
        build_remaps(&out_of_range, &[2], &[0]),
        Err(MnemeError::Corrupted { .. })
    ));

    // 槽位重复:两条版本行同占 slot 0。
    let duplicate = [(version(1, 0), 0), (version(2, 0), 0)];
    assert!(matches!(
        build_remaps(&duplicate, &[2], &[0]),
        Err(MnemeError::Corrupted { .. })
    ));

    // 未被引用:row_count = 2 但只有 slot 0 出现。
    let unreferenced = [(version(1, 0), 0)];
    assert!(matches!(
        build_remaps(&unreferenced, &[2], &[0]),
        Err(MnemeError::Corrupted { .. })
    ));
}
