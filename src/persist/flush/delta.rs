//! 访问/关系增量条目个收集(段物化 delta 区)。

use crate::core::bitset::BitSet;
use crate::memory::table::WriterState;
use crate::persist::msec::DeltaEntry;

/// 由写状态构造本次段物化需要的 delta 条目(访问计数 + 关系边净变更)。
///
/// `full_relations` 为真(首段/compaction)时关系表全量重写,不再产出关系 delta;
/// `included` 中已有最新版本的 RowId,其访问统计随记录体列落盘,无需 Access 条目。
pub(crate) fn build_delta(
    ws: &WriterState,
    included: &[usize],
    now_ms: i64,
    full_relations: bool,
) -> Vec<DeltaEntry> {
    let mut in_segment = BitSet::default();
    for &idx in included {
        in_segment.set(idx);
    }
    let mut entries = Vec::new();
    collect_access_deltas(ws, &in_segment, now_ms, &mut entries);
    if !full_relations {
        collect_edge_deltas(ws, now_ms, &mut entries);
    }
    entries
}

/// 访问增量条目:最新版本随本段物化时跳过(记录体已携带统计)。
fn collect_access_deltas(
    ws: &WriterState,
    in_segment: &BitSet,
    now_ms: i64,
    entries: &mut Vec<DeltaEntry>,
) {
    for (&rowid, &access_delta) in ws.access_dirty.iter() {
        // 最新版本随本段落盘时,记录体的 access 列已携带最新统计。
        if let Some(latest) = ws.latest.get(&rowid)
            && in_segment.get(latest.get() as usize)
        {
            continue;
        }
        let stat = ws.access.get(&rowid).copied().unwrap_or_default();
        let ns_id = ns_of(ws, rowid);
        entries.push(DeltaEntry::Access {
            seqno: ws.seqno.get(),
            tx_ms: now_ms,
            ns_id,
            rowid: rowid.get(),
            last_access_ms: stat.last_access_ms,
            access_delta,
            importance_delta: 0.0,
        });
    }
}

/// 关系净变更条目(全量重写关系表时不需要)。
fn collect_edge_deltas(ws: &WriterState, now_ms: i64, entries: &mut Vec<DeltaEntry>) {
    for &(from, to, kind) in ws.edge_dirty.iter() {
        let edge = ws.out_edges.get(&from).and_then(|edges| {
            edges
                .iter()
                .find(|edge| edge.to == to && edge.kind.0 == kind)
        });
        let ns_id = ns_of(ws, from);
        entries.push(match edge {
            Some(edge) => DeltaEntry::Relate {
                seqno: ws.seqno.get(),
                tx_ms: now_ms,
                ns_id,
                from: from.get(),
                to: to.get(),
                kind,
                weight: edge.weight,
                meta: edge.metadata.clone(),
            },
            None => DeltaEntry::Unrelate {
                seqno: ws.seqno.get(),
                tx_ms: now_ms,
                ns_id,
                from: from.get(),
                to: to.get(),
                kind,
            },
        });
    }
}

/// 该 RowId 最新版本所属命名空间;不存在时为 0。
fn ns_of(ws: &WriterState, rowid: crate::core::types::RowId) -> u32 {
    ws.latest
        .get(&rowid)
        .map_or(0, |slot| ws.slots[slot.get() as usize].ns_id.get())
}
