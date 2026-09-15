//! 关系边重建同 delta 回放。

use crate::core::error::Result;
use crate::core::options::RelationKind;
use crate::core::types::RowId;
use crate::memory::relation::{self, Edge};
use crate::memory::table::WriterState;
use crate::persist::msec;

/// 从单个段的 relations 区重建关系边(出边 + 入边)。
///
/// 带 `FLAG_FULL` 的段(首段/compaction 段)先重置关系表再应用;增量段仅 upsert,
/// 其后由 delta 区施加关系变更——增量段的 relations 区是空表,按全量重置会清掉
/// 先前段的边。全量语义保证 compaction 后已删除的边不会因并集而"复活"
/// (设计 04 §2.2b、FC-MODEL-POST-007)。
///
/// `RelationIndex::Both` 的段带反向表:入边由反向表 + 正向表并集恢复
/// (并集对损坏文件更稳健,upsert 幂等)。
pub(in crate::persist::recover) fn apply_relations(
    state: &mut WriterState,
    msec_view: &msec::MsecView<'_>,
) -> Result<()> {
    let edges = crate::persist::edges::parse(msec_view.relations_bytes())?;
    if edges.full {
        state.out_edges = crate::core::sharded::ShardedMap::new();
        state.in_edges = crate::core::sharded::ShardedMap::new();
    }
    let build = |edge: &crate::persist::edges::EdgeData| Edge {
        from: RowId::new(edge.from),
        to: RowId::new(edge.to),
        kind: RelationKind(edge.kind),
        weight: edge.weight,
        metadata: edge.meta.clone(),
    };
    for edge in &edges.forward {
        relation::upsert_edge_sharded(&mut state.out_edges, build(edge));
        relation::upsert_edge_sharded(&mut state.in_edges, build(edge));
    }
    for edge in &edges.reverse {
        relation::upsert_edge_sharded(&mut state.in_edges, build(edge));
    }
    Ok(())
}

/// 从单个段的 delta 区回放跨段访问统计与关系变更(设计 04 §2.2a)。
///
/// 访问统计按"版本行累计快照"语义恢复:delta 的 `seqno` 早于该 RowId 最新版本行
/// 的 `seqno` 时,说明后续版本行的 `access` 列已包含该增量,必须跳过,否则重开后
/// 计数虚高(FC-PERSIST-POST-010)。关系边是操作语义(upsert/remove),不参与该判定。
pub(in crate::persist::recover) fn apply_delta(
    state: &mut WriterState,
    msec_view: &msec::MsecView<'_>,
) -> Result<()> {
    for entry in msec::decode_delta(msec_view.delta_bytes())? {
        apply_delta_entry(state, entry);
    }
    Ok(())
}

/// 应用单条 delta 条目。
fn apply_delta_entry(state: &mut WriterState, entry: msec::DeltaEntry) {
    match entry {
        msec::DeltaEntry::Access {
            seqno,
            rowid,
            last_access_ms,
            access_delta,
            ..
        } => {
            if superseded_by_latest_version(state, rowid, seqno) {
                return;
            }
            let stat = state.access.get_or_insert_default(RowId::new(rowid));
            stat.access_count = stat.access_count.saturating_add(access_delta);
            // 时钟回拨下回放序可能倒退;保留更晚的访问时刻。
            stat.last_access_ms = stat.last_access_ms.max(last_access_ms);
        }
        msec::DeltaEntry::Relate {
            from,
            to,
            kind,
            weight,
            meta,
            ..
        } => {
            let edge = Edge {
                from: RowId::new(from),
                to: RowId::new(to),
                kind: RelationKind(kind),
                weight,
                metadata: meta,
            };
            relation::upsert_edge_sharded(&mut state.out_edges, edge.clone());
            relation::upsert_edge_sharded(&mut state.in_edges, edge);
        }
        msec::DeltaEntry::Unrelate { from, to, kind, .. } => {
            apply_unrelate_delta(state, from, to, kind);
        }
    }
}

/// 应用 `Unrelate` delta:移除出边与入边。
fn apply_unrelate_delta(state: &mut WriterState, from: u64, to: u64, kind: u16) {
    relation::remove_edge_sharded(
        &mut state.out_edges,
        RowId::new(from),
        RowId::new(to),
        RelationKind(kind),
    );
    relation::remove_edge_sharded(
        &mut state.in_edges,
        RowId::new(to),
        RowId::new(from),
        RelationKind(kind),
    );
}

/// `Access` delta 是否已被该 RowId 更晚的版本行覆盖。
///
/// 版本行的 `access` 列是写入时刻的累计快照;delta 的 `seqno` 早于最新版本行的
/// `seqno` 时,该增量已计入版本行,不可再累加(FC-PERSIST-POST-010)。
/// RowId 无版本行(例如版本被回收)时返回 `false`,delta 必须照常应用。
fn superseded_by_latest_version(state: &WriterState, rowid: u64, delta_seqno: u64) -> bool {
    state
        .latest
        .get(&RowId::new(rowid))
        .is_some_and(|slot| state.slots[slot.get() as usize].seqno.get() > delta_seqno)
}
