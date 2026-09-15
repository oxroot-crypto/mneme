use std::sync::Arc;

use crate::core::error::MnemeError;
use crate::core::types::SeqNo;

use super::*;

/// FC-MEM-INV-004
#[test]
fn slot_id_for_rejects_overflow() {
    assert_eq!(slot_id_for(0).expect("0 合法").get(), 0);
    assert_eq!(
        slot_id_for(u32::MAX as usize).expect("上界合法").get(),
        u32::MAX
    );
    let overflow = u32::MAX as usize + 1;
    assert!(matches!(
        slot_id_for(overflow),
        Err(MnemeError::LimitExceeded {
            field: "slots",
            limit,
            got,
        }) if limit == u32::MAX as usize && got == overflow
    ));
}

/// FC-PERSIST-INV-020 / FC-PERSIST-ERR-012:`RowId`/`NsId`/`SeqNo`
/// 空间耗尽时返回 `IdExhausted`,绝不回绕复用。
#[test]
fn id_allocators_reject_exhaustion() {
    let mut state = WriterState::new();
    state.next_rowid = u64::MAX;
    assert!(matches!(
        state.alloc_rowid(),
        Err(MnemeError::IdExhausted { kind: "rowid" })
    ));
    state.seqno = SeqNo::new(u64::MAX);
    assert!(matches!(
        state.alloc_seqno(),
        Err(MnemeError::IdExhausted { kind: "seqno" })
    ));
    state.next_ns_id = u32::MAX;
    assert!(matches!(
        state.register_ns("耗尽"),
        Err(MnemeError::IdExhausted { kind: "ns_id" })
    ));
}

/// FC-MODEL-POST-008:编号空间耗尽 → `TooLarge`;恢复登记冲突/越界 → `Corrupted`。
#[test]
fn relation_kind_registry_rejects_exhaustion() {
    let mut state = WriterState::new();
    state.next_rel_kind = u16::MAX;
    assert!(matches!(
        state.register_relation_kind("耗尽"),
        Err(MnemeError::TooLarge {
            field: "rel_kind",
            ..
        })
    ));

    let mut state = WriterState::new();
    state
        .register_recovered_rel_kind(16, Arc::from("mentions"))
        .expect("首次登记");
    assert_eq!(state.next_rel_kind, 17, "水位必须推进");
    assert!(matches!(
        state.register_recovered_rel_kind(17, Arc::from("mentions")),
        Err(MnemeError::Corrupted { .. })
    ));
    assert!(matches!(
        state.register_recovered_rel_kind(16, Arc::from("other")),
        Err(MnemeError::Corrupted { .. })
    ));
    assert!(matches!(
        state.register_recovered_rel_kind(3, Arc::from("builtin")),
        Err(MnemeError::Corrupted { .. })
    ));
    assert!(matches!(
        state.register_recovered_rel_kind(18, Arc::from("")),
        Err(MnemeError::Corrupted { .. })
    ));
}
