//! `flush` 段物化个单元测试。

use super::*;
use std::time::Duration;

use crate::core::meta::json;
use crate::core::metric::Metric;
use crate::core::options::{
    CompactionPolicy, Compression, Dimension, HnswParams, InsertMode, Limits, RelationIndex,
    SystemClock, Tuning, VectorFormat,
};
use crate::core::types::{Key, NsId, RowId, SeqNo};
use crate::memory::dedup::Dedup;
use crate::memory::table::SlotData;

/// 构造仅 `tuning.field_dict_max` 等字段有意义的最小运行配置。
fn test_config() -> Config {
    Config {
        dimension: Dimension::new(2).expect("dimension"),
        metric: Metric::Cosine,
        insert_mode: InsertMode::default(),
        dedup: Dedup::default(),
        dedup_threshold: 0.9,
        quantization: VectorFormat::default(),
        hnsw: HnswParams::default(),
        build_precision: crate::core::options::BuildPrecision::default(),
        index_factory: None,
        compaction: CompactionPolicy::default(),
        retention: None,
        retain_interval: None,
        access_flush_interval: Duration::from_secs(60),
        compression: Compression::default(),
        observer: None,
        relation_index: RelationIndex::default(),
        parallelism: 1,
        tuning: Tuning::default(),
        limits: Limits::default(),
        clock: Arc::new(SystemClock),
        read_only: false,
    }
}

/// 构造一条 metadata 里带数值 `key` 的槽位(仅字段字典去重测试用)。
fn slot_with_numeric_key() -> Arc<SlotData> {
    Arc::new(SlotData {
        rowid: RowId::new(0),
        ns_id: NsId::new(1),
        ns_path: Arc::from("n"),
        seqno: SeqNo::new(1),
        key: Some(Key::new("k0")),
        vector: crate::memory::lazy::VectorStorage::owned(Arc::from(
            vec![0.0_f32, 1.0].into_boxed_slice(),
        )),
        norm_sq: 1.0,
        text: None,
        text_hash: None,
        meta: json!({"key": 7}),
        created_at: 1_000,
        expires_at: None,
        importance: 0.5,
        confidence: 1.0,
        valid_from: 1_000,
        valid_to: None,
        provenance: None,
        tx_ms: 1_000,
        deleted: false,
    })
}

/// FC-PERSIST-POST-008(字段字典 `key` 去重:metadata 数值 `key` 不得与 bloom 保留字段重名)
#[test]
fn field_dict_keeps_single_key_field() {
    let mut ws = WriterState::new();
    let slot = slot_with_numeric_key();
    Arc::make_mut(&mut ws.slots).push(Arc::clone(&slot));
    Arc::make_mut(&mut ws.zones).observe(0, &slot);
    let blobs = build_indexes(&ws, &test_config(), &[0]).expect("build_indexes");
    let defs = msec::decode_field_dict(&blobs.field_dict).expect("field_dict");
    let key_fields: Vec<_> = defs
        .iter()
        .filter(|field| field.name.as_ref() == "key")
        .collect();
    assert_eq!(key_fields.len(), 1, "字段字典中 `key` 必须唯一");
    assert_eq!(
        key_fields[0].kind,
        msec::FieldKind::Str,
        "唯一 `key` 必须是 bloom 用的字符串保留字段"
    );
    let blooms = msec::decode_bloom(&blobs.bloom).expect("bloom");
    assert_eq!(blooms.len(), 1);
    assert_eq!(
        blooms[0].0, key_fields[0].id,
        "bloom 字段编号必须指向唯一的 `key`"
    );
}
