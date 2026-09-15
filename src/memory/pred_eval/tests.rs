use std::sync::Arc;

use super::*;

use crate::core::meta::Meta;
use crate::memory::pred::{EvalCtx, Expr};
use crate::memory::table::{AccessStat, SlotData};

fn ctx_with(meta: Meta) -> (SlotData, AccessStat) {
    let slot = SlotData {
        rowid: crate::core::types::RowId::new(1),
        ns_id: crate::core::types::NsId::new(1),
        ns_path: Arc::from("a"),
        seqno: crate::core::types::SeqNo::new(1),
        key: Some(crate::core::types::Key::new("k")),
        vector: crate::memory::lazy::VectorStorage::owned(Arc::from(
            vec![0.0_f32].into_boxed_slice(),
        )),
        norm_sq: 0.0,
        text: Some(Arc::from("hello world")),
        text_hash: None,
        meta,
        created_at: 10,
        expires_at: None,
        importance: 0.5,
        confidence: 1.0,
        valid_from: 10,
        valid_to: None,
        provenance: None,
        tx_ms: 10,
        deleted: false,
    };
    (slot, AccessStat::default())
}

fn hit(expr: &Expr, slot: &SlotData, access: AccessStat) -> bool {
    matches(
        expr,
        &EvalCtx {
            slot,
            access: Some(access),
        },
    )
}

/// FC-QUERY-POST-001
#[test]
fn glob_wildcards() {
    assert!(glob_match("a*c", "abbbc"));
    assert!(glob_match("a?c", "abc"));
    assert!(!glob_match("a?c", "abbc"));
    assert!(glob_match("*", ""));
    assert!(glob_match("", ""));
}

/// FC-QUERY-ERR-002
#[test]
fn missing_field_keeps_not_false() {
    let (slot, access) = ctx_with(crate::core::meta::json!({}));
    let expr = Expr::Not(Box::new(Expr::field("kind").eq("x")));
    assert!(!hit(&expr, &slot, access));
    assert!(!hit(&Expr::Exists("kind".into()), &slot, access));
}

/// FC-QUERY-POST-001(保留字段优先于同名 metadata)
#[test]
fn reserved_fields_shadow_metadata() {
    let (slot, access) = ctx_with(crate::core::meta::json!({"importance": 0.0}));
    assert!(hit(&Expr::field("importance").gt(0.4), &slot, access));
}

/// FC-QUERY-POST-005(保留名清单与 `resolve` 的保留分支保持同步)
#[test]
fn reserved_names_resolve_to_reserved_values() {
    let (mut slot, mut access) = ctx_with(crate::core::meta::json!({}));
    slot.key = Some(crate::core::types::Key::new("k"));
    slot.expires_at = Some(1);
    slot.valid_to = Some(2);
    access.last_access_ms = 3;
    access.access_count = 4;
    for name in RESERVED_FIELDS {
        assert!(is_reserved_field(name));
        match resolve(
            name,
            &EvalCtx {
                slot: &slot,
                access: Some(access),
            },
        ) {
            Some(FieldValue::Reserved(_) | FieldValue::Text(_)) => {}
            Some(FieldValue::Meta(_)) => panic!("{name} 落入 metadata 分支"),
            None => panic!("{name} 未按保留值解析"),
        }
    }
}
