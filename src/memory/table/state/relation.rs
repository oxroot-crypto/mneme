//! 关系类型注册表与关系边的建立/删除。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::options::RelationKind;
use crate::core::types::RowId;
use crate::memory::relation::Edge;
use crate::memory::table::write_op::WriteOp;

use super::WriterState;

/// 自定义关系类型名称最大字节数(防注册表被超长名称膨胀)。
const REL_KIND_NAME_MAX: usize = 128;

/// 内置关系类型的名称解析(与 [`RelationKind`] 常量同名,便于宿主用字符串配置)。
fn builtin_relation_kind(name: &str) -> Option<RelationKind> {
    match name {
        "derived_from" => Some(RelationKind::DERIVED_FROM),
        "supports" => Some(RelationKind::SUPPORTS),
        "contradicts" => Some(RelationKind::CONTRADICTS),
        "related" => Some(RelationKind::RELATED),
        _ => None,
    }
}

impl WriterState {
    /// 注册/解析一个自定义关系类型名称(编号单调分配、永不复用)。
    ///
    /// 内置名(`derived_from`/`supports`/`contradicts`/`related`)解析为内置编号;
    /// 同名重复调用幂等返回既有编号。名称须非空、≤ 128 字节、不含控制字符。
    ///
    /// # Errors
    /// - 名称为空/超长/含控制字符 → [`MnemeError::Config`];
    /// - 编号空间耗尽(`next_rel_kind == u16::MAX`)→ [`MnemeError::TooLarge`];
    /// - `SeqNo` 空间耗尽 → [`MnemeError::IdExhausted`]。
    pub(crate) fn register_relation_kind(&mut self, name: &str) -> Result<RelationKind> {
        if let Some(kind) = builtin_relation_kind(name) {
            return Ok(kind);
        }
        if let Some(&kind) = self.rel_kinds.get(name) {
            return Ok(RelationKind(kind));
        }
        if name.is_empty() || name.len() > REL_KIND_NAME_MAX || name.chars().any(char::is_control) {
            return Err(MnemeError::Config {
                reason: "关系类型名须非空、≤128 字节且不含控制字符",
            });
        }
        if self.next_rel_kind == u16::MAX {
            return Err(MnemeError::TooLarge {
                field: "rel_kind",
                limit: u16::MAX as usize,
                got: self.next_rel_kind as usize,
            });
        }
        let kind = self.next_rel_kind;
        self.next_rel_kind += 1;
        let name: Arc<str> = Arc::from(name);
        Arc::make_mut(&mut self.rel_kinds).insert(Arc::clone(&name), kind);
        Arc::make_mut(&mut self.rel_kind_names).insert(kind, Arc::clone(&name));
        let seqno = self.alloc_seqno()?;
        self.pending
            .push(WriteOp::RelKindRegister { kind, name, seqno });
        Ok(RelationKind(kind))
    }

    /// 恢复期登记一个自定义关系类型(来自 MANIFEST 或 WAL 帧)。
    ///
    /// 编号必须 ≥ [`RelationKind::FIRST_CUSTOM`];同名不同号或同号不同名一律
    /// [`MnemeError::Corrupted`],绝不静默改写注册表;水位推进失败同样拒绝
    /// (FC-MODEL-POST-008、FC-PERSIST-ERR-012)。
    ///
    /// # Errors
    /// 见上。
    pub(crate) fn register_recovered_rel_kind(&mut self, kind: u16, name: Arc<str>) -> Result<()> {
        let corrupt = |reason: &str| MnemeError::Corrupted {
            segment: None,
            reason: format!("rel_kind 注册表:{reason}"),
        };
        if kind < RelationKind::FIRST_CUSTOM || name.is_empty() {
            return Err(corrupt("编号低于自定义起点或名称为空"));
        }
        if let Some(existing) = self.rel_kinds.get(&name).copied()
            && existing != kind
        {
            return Err(corrupt("同名不同编号"));
        }
        if let Some(existing) = self.rel_kind_names.get(&kind)
            && existing.as_ref() != name.as_ref()
        {
            return Err(corrupt("同编号不同名"));
        }
        Arc::make_mut(&mut self.rel_kinds).insert(Arc::clone(&name), kind);
        Arc::make_mut(&mut self.rel_kind_names).insert(kind, name);
        let next = kind
            .checked_add(1)
            .ok_or_else(|| corrupt("编号已达 u16::MAX"))?;
        if next > self.next_rel_kind {
            self.next_rel_kind = next;
        }
        Ok(())
    }

    /// 建立/更新关系边并记录 WAL 操作。
    ///
    /// # Errors
    /// `SeqNo` 空间耗尽时返回 [`MnemeError::IdExhausted`]。
    pub(crate) fn relate_edge(&mut self, edge: Edge) -> Result<()> {
        crate::memory::relation::upsert_edge_sharded(&mut self.out_edges, edge.clone());
        crate::memory::relation::upsert_edge_sharded(&mut self.in_edges, edge.clone());
        Arc::make_mut(&mut self.edge_dirty).insert((edge.from, edge.to, edge.kind.0));
        let seqno = self.alloc_seqno()?;
        self.pending.push(WriteOp::Relate {
            from: edge.from,
            to: edge.to,
            kind: edge.kind.0,
            weight: edge.weight,
            meta: edge.metadata,
            seqno,
        });
        Ok(())
    }

    /// 删除关系边并记录 WAL 操作;未命中返回 `false` 且不消耗序号。
    ///
    /// # Errors
    /// 命中边且 `SeqNo` 空间耗尽时返回 [`MnemeError::IdExhausted`]。
    pub(crate) fn unrelate_edge(
        &mut self,
        from: RowId,
        to: RowId,
        kind: RelationKind,
    ) -> Result<bool> {
        let removed =
            crate::memory::relation::remove_edge_sharded(&mut self.out_edges, from, to, kind);
        crate::memory::relation::remove_edge_sharded(&mut self.in_edges, to, from, kind);
        if removed {
            Arc::make_mut(&mut self.edge_dirty).insert((from, to, kind.0));
            let seqno = self.alloc_seqno()?;
            self.pending.push(WriteOp::Unrelate {
                from,
                to,
                kind: kind.0,
                seqno,
            });
        }
        Ok(removed)
    }
}
