use super::*;
use crate::core::meta::Meta;
use crate::core::meta::json;
use crate::core::types::{Key, NsId, RowId, SeqNo};
use crate::memory::table::SlotData;
use std::sync::Arc;

fn slot(index: usize, importance: f32, meta: Meta) -> SlotData {
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
        created_at: 1_000 + index as i64,
        expires_at: None,
        importance,
        confidence: 1.0,
        valid_from: 1_000,
        valid_to: None,
        provenance: None,
        tx_ms: 1_000,
        deleted: false,
    }
}

#[test]
fn aggregates_min_max_within_block() {
    let mut zones = ZoneIndex::new(16);
    zones.observe(0, &slot(0, 0.2, json!({"rank": 7})));
    zones.observe(1, &slot(1, 0.9, json!({"rank": 3})));
    let stat = zones.block_stat("importance", 0).expect("importance");
    assert_eq!(
        (stat.min, stat.max),
        (f64::from(0.2_f32), f64::from(0.9_f32))
    );
    let rank = zones.block_stat("rank", 0).expect("rank");
    assert_eq!((rank.min, rank.max), (3.0, 7.0));
}

#[test]
fn separates_blocks() {
    let mut zones = ZoneIndex::new(16);
    zones.observe(0, &slot(0, 0.1, json!({})));
    zones.observe(ZONE_BLOCK_ROWS, &slot(1, 0.7, json!({})));
    assert_eq!(
        zones.block_stat("importance", 0).map(|s| s.min),
        Some(f64::from(0.1_f32))
    );
    assert_eq!(
        zones.block_stat("importance", 1).map(|s| s.min),
        Some(f64::from(0.7_f32))
    );
    assert!(zones.block_stat("importance", 2).is_none());
}

#[test]
fn marks_null_and_absent_values() {
    let mut zones = ZoneIndex::new(16);
    zones.observe(0, &slot(0, 0.5, json!({"note": null})));
    let note = zones.block_stat("note", 0).expect("note");
    assert!(note.has_null);
    assert!(!note.has_value);
    assert!(note.has_any, "null 也是存在的取值");
    assert!(zones.field_zones("note").is_some());
}

#[test]
fn marks_existence_for_non_numeric_values() {
    let mut zones = ZoneIndex::new(16);
    zones.observe(0, &slot(0, 0.5, json!({"note": "s", "flag": true})));
    let note = zones.block_stat("note", 0).expect("note");
    assert!(note.has_any, "字符串字段必须记录存在性");
    assert!(!note.has_value, "字符串不参与数值区间");
    assert!(!note.has_null);
    let flag = zones.block_stat("flag", 0).expect("flag");
    assert!(flag.has_any, "布尔字段必须记录存在性");
}

/// 大整数(>2^53)无法用 `f64` 精确表示:区间的放宽为 ±∞,绝不误剪。
#[test]
fn large_integers_widen_block_interval() {
    let mut zones = ZoneIndex::new(16);
    zones.observe(0, &slot(0, 0.5, json!({"rank": 9_007_199_254_740_993_i64})));
    let rank = zones.block_stat("rank", 0).expect("rank");
    assert!(rank.has_value);
    assert_eq!((rank.min, rank.max), (f64::NEG_INFINITY, f64::INFINITY));
}

/// FC-QUERY-POST-005(字段类别冲突必须放弃剪枝,不得静默丢弃统计)
#[test]
fn kind_conflict_disables_block_pruning() {
    let mut zones = ZoneIndex::new(16);
    zones.observe_int("conflict", ZoneKind::Ts, 1_000, 0);
    assert_eq!(
        zones.field_zones("conflict").map(|(_, kind)| kind),
        Some(ZoneKind::Ts)
    );
    zones.observe_int("conflict", ZoneKind::Num, 7, 0);
    assert_eq!(
        zones.field_zones("conflict").map(|(_, kind)| kind),
        None,
        "类型冲突字段必须退出块级剪枝"
    );
}

#[test]
fn respects_field_limit() {
    // 保留字段占 4 个(created_at/importance/confidence/valid_from),
    // 上限 5 时恰好还能注册一个 metadata 字段(a 与 b 按字典序,a 先)。
    let mut zones = ZoneIndex::new(5);
    zones.observe(0, &slot(0, 0.5, json!({"a": 1, "b": 2})));
    assert!(zones.field_zones("a").is_some());
    assert!(zones.field_zones("b").is_none(), "超上限字段不再注册");
}

/// FC-QUERY-POST-005(保留字段同名的 metadata 不进 zone map,行级以保留值为准)
#[test]
fn reserved_metadata_is_shadowed_and_not_indexed() {
    let mut zones = ZoneIndex::new(16);
    zones.observe(
        0,
        &slot(0, 0.5, json!({"created_at": 5, "key": 9, "rowid": 99})),
    );
    assert_eq!(
        zones.field_zones("created_at").map(|(_, kind)| kind),
        Some(ZoneKind::Ts),
        "created_at 的统计只来自保留值"
    );
    assert!(zones.field_zones("key").is_none(), "保留名 metadata 不注册");
    assert!(
        zones.field_zones("rowid").is_none(),
        "保留名 metadata 不注册"
    );
    let stat = zones.block_stat("created_at", 0).expect("created_at");
    assert!(stat.min >= 1_000.0, "不得混入 metadata 的 5");
}

/// FC-QUERY-POST-005(保留名对象下的子路径照常观察,如 `key.x`)
#[test]
fn reserved_object_subpaths_are_still_indexed() {
    let mut zones = ZoneIndex::new(16);
    zones.observe(0, &slot(0, 0.5, json!({"key": {"x": 5}})));
    assert!(zones.field_zones("key").is_none(), "保留名自身仍不注册");
    let stat = zones.block_stat("key.x", 0).expect("key.x 子路径");
    assert_eq!((stat.min, stat.max), (5.0, 5.0));
}
