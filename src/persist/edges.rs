//! 关系邻接索引(`relations`)编解码(设计 04 §2.2b)。
//!
//! 边表按 `(from, kind, to)` 排序,供 `O(log n + degree)` 定位出边;
//! [`RelationIndex::Both`] 时追加按 `(to, kind, from)` 排序的反向表。区首头部记录
//! 两段长度,正向/反向表各自完整排序。
//!
//! ```text
//! 0  magic "EDG1" | 4 u16 ver | 6 u16 flags | 8 u32 forward_count | 12 u32 reverse_count
//! 正向边 × forward_count:  [from u64][to u64][kind u16][f32 weight][meta u32 len+bytes]
//! 反向边 × reverse_count(仅 flags bit0):
//!                          [from u64][to u64][kind u16][f32 weight][meta u32 len+bytes]
//! ```

use crate::core::error::{MnemeError, Result};
use crate::core::meta::{self, Meta};
use crate::persist::{
    Cursor, FORMAT_VERSION, check_version, put_bytes_u32, put_u16, put_u32, put_u64,
};

/// 关系区魔数。
pub(crate) const MAGIC: [u8; 4] = *b"EDG1";
/// 反向表存在标志。
const FLAG_REVERSE: u16 = 1 << 0;
/// 关系区为**全量快照**的标志:恢复时先重置关系表再应用;无此位的段为增量段,
/// 按 upsert 处理(其 relations 区为空表,关系变更由 delta 区承载,
/// 见 FC-MODEL-POST-007)。
const FLAG_FULL: u16 = 1 << 1;
/// 单条边的最小编码字节数:`from(8)+to(8)+kind(2)+weight(4)+meta 长度(4)`。
/// 用于按剩余字节数为边数预分配设上界,避免损坏文件的大计数触发超额分配。
const MIN_EDGE_BYTES: usize = 26;

/// 一条关系边。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EdgeData {
    /// 起点 RowId。
    pub(crate) from: u64,
    /// 终点 RowId。
    pub(crate) to: u64,
    /// 关系类型编号。
    pub(crate) kind: u16,
    /// 边权。
    pub(crate) weight: f32,
    /// 边元数据(JSON)。
    pub(crate) meta: Meta,
}

/// 编码关系区;`reverse = true` 时追加反向邻接表,`full = true` 时标记为全量快照。
///
/// # Errors
/// 正向/反向边数超过 `u32::MAX` 时返回 [`MnemeError::LimitExceeded`],绝不静默
/// 截断计数(截断会让解析端把多余边判为尾部残留)。
pub(crate) fn encode(edges: &[EdgeData], reverse: bool, full: bool) -> Result<Vec<u8>> {
    let mut forward: Vec<&EdgeData> = edges.iter().collect();
    forward.sort_by_key(|edge| (edge.from, edge.kind, edge.to));
    let mut reverse_edges: Vec<&EdgeData> = Vec::new();
    if reverse {
        // 去重同 (to, kind, from):同一对端只出现一次。
        reverse_edges = edges.iter().collect();
        reverse_edges.sort_by_key(|edge| (edge.to, edge.kind, edge.from));
        reverse_edges.dedup_by_key(|edge| (edge.to, edge.kind, edge.from));
    }
    let forward_count = u32::try_from(forward.len()).map_err(|_| MnemeError::LimitExceeded {
        field: "relations forward",
        limit: u32::MAX as usize,
        got: forward.len(),
    })?;
    let reverse_count =
        u32::try_from(reverse_edges.len()).map_err(|_| MnemeError::LimitExceeded {
            field: "relations reverse",
            limit: u32::MAX as usize,
            got: reverse_edges.len(),
        })?;

    let mut flags = 0_u16;
    if reverse {
        flags |= FLAG_REVERSE;
    }
    if full {
        flags |= FLAG_FULL;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    put_u16(&mut out, FORMAT_VERSION);
    put_u16(&mut out, flags);
    put_u32(&mut out, forward_count);
    put_u32(&mut out, reverse_count);
    for edge in forward {
        encode_edge(&mut out, edge);
    }
    for edge in reverse_edges {
        encode_edge(&mut out, edge);
    }
    Ok(out)
}

/// 追加一条边(含 meta 长度前缀)。
fn encode_edge(out: &mut Vec<u8>, edge: &EdgeData) {
    put_u64(out, edge.from);
    put_u64(out, edge.to);
    put_u16(out, edge.kind);
    out.extend_from_slice(&edge.weight.to_le_bytes());
    let meta_bytes = meta::to_bytes(&edge.meta);
    put_bytes_u32(out, &meta_bytes);
}

/// 解析后的关系区视图。
pub(crate) struct EdgeView {
    /// 正向边(`(from, kind, to)` 升序)。
    pub(crate) forward: Vec<EdgeData>,
    /// 反向边(`(to, kind, from)` 升序);未存储时为空。
    ///
    /// `RelationIndex::Both` 段写入;恢复时与正向表并集重建 `in_edges`
    /// (FC-MODEL-POST-007)。
    pub(crate) reverse: Vec<EdgeData>,
    /// 是否为全量关系表(`FLAG_FULL`);恢复时应先重置关系表再应用。
    pub(crate) full: bool,
}

/// 校验并解析关系区。
///
/// # Errors
/// 魔数/版本不符、长度不足或 meta 损坏时返回结构化错误。
pub(crate) fn parse(bytes: &[u8]) -> Result<EdgeView> {
    if bytes.is_empty() {
        return Ok(EdgeView {
            forward: Vec::new(),
            reverse: Vec::new(),
            full: false,
        });
    }
    let mut cursor = Cursor::new(bytes, "relations");
    if cursor.take(4)? != MAGIC {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "relations: 魔数不符".to_string(),
        });
    }
    check_version("relations", cursor.u16()?)?;
    let flags = cursor.u16()?;
    let forward_count = cursor.u32()? as usize;
    let reverse_count = cursor.u32()? as usize;
    let read_edges = |cursor: &mut Cursor<'_>, count: usize| -> Result<Vec<EdgeData>> {
        // 上界预分配:每条边至少 MIN_EDGE_BYTES,损坏计数不会超额分配。
        let mut edges = Vec::with_capacity(count.min(cursor.remaining() / MIN_EDGE_BYTES));
        for _ in 0..count {
            edges.push(decode_edge(cursor)?);
        }
        Ok(edges)
    };
    let forward = read_edges(&mut cursor, forward_count)?;
    let reverse = if flags & FLAG_REVERSE != 0 {
        read_edges(&mut cursor, reverse_count)?
    } else {
        Vec::new()
    };
    if !cursor.is_empty() {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "relations: 区尾有残留字节".to_string(),
        });
    }
    Ok(EdgeView {
        forward,
        reverse,
        full: flags & FLAG_FULL != 0,
    })
}

/// 解码单条边。
fn decode_edge(cursor: &mut Cursor<'_>) -> Result<EdgeData> {
    let from = cursor.u64()?;
    let to = cursor.u64()?;
    let kind = cursor.u16()?;
    let weight = f32::from_le_bytes(cursor.take(4)?.try_into().unwrap_or([0; 4]));
    let meta_len = cursor.u32()? as usize;
    let meta = meta::from_bytes(cursor.take(meta_len)?)?;
    Ok(EdgeData {
        from,
        to,
        kind,
        weight,
        meta,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::meta::json;

    fn edge(from: u64, to: u64, kind: u16) -> EdgeData {
        EdgeData {
            from,
            to,
            kind,
            weight: 0.5,
            meta: json!({"why": "test"}),
        }
    }

    /// 正向排序 + 可选反向表往返一致。
    #[test]
    fn edges_roundtrip_with_reverse() {
        let edges = vec![edge(2, 1, 3), edge(1, 2, 0), edge(1, 3, 0)];
        let bytes = encode(&edges, true, true).expect("encode");
        let view = parse(&bytes).expect("parse");
        assert_eq!(view.forward.len(), 3);
        // 正向按 (from,kind,to):(1,0,2),(1,0,3),(2,3,1)。
        assert_eq!((view.forward[0].from, view.forward[0].to), (1, 2));
        assert_eq!((view.forward[1].from, view.forward[1].to), (1, 3));
        assert_eq!((view.forward[2].from, view.forward[2].to), (2, 1));
        // 反向按 (to,kind,from):(1,3,2),(2,0,1),(3,0,1)。
        assert_eq!(view.reverse.len(), 3);
        assert_eq!(view.reverse[0].to, 1);
    }

    /// 空区解析为零边。
    #[test]
    fn edges_empty_is_valid() {
        let view = parse(&[]).expect("parse");
        assert!(view.forward.is_empty());
        assert!(view.reverse.is_empty());
    }

    /// 魔数损坏被检出。
    #[test]
    fn edges_detects_bad_magic() {
        let mut bytes = encode(&[edge(1, 2, 0)], false, false).expect("encode");
        bytes[0] = b'X';
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// FC-PERSIST-ERR-011(口径一致):区尾残留字节 → `Corrupted`,绝不静默忽略。
    #[test]
    fn edges_reject_trailing_bytes() {
        let mut bytes = encode(&[edge(1, 2, 0)], false, false).expect("encode");
        bytes.push(0xAB);
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }
}
