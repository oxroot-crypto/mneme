//! msec `delta` 区(跨段覆盖)编解码(设计 04 §2.2a)。
//!
//! 增量段只物化新版本槽位;对**已落盘旧段**的非版本化变更(访问统计、关系边)
//! 以 delta 条目承载,恢复时按段序回放。完整新版本(含 `touch` boost 产生的
//! 版本)仍走 `version_table`,不重复登记。
//!
//! ```text
//! delta: [magic "DLT1"][u16 ver][u16 count][u32 entry_crc32]
//!        条目 × count(按 (target, seqno, kind) 排序):
//!          [u8 kind][u64 seqno][i64 tx_ms][u32 ns_id]
//!          kind=4 Access  : [RowId u64][i64 last_access_ms][u32 access_delta][f32 importance_delta]
//!          kind=5 Relate  : [from u64][to u64][kind u16][f32 weight][meta len+bytes]
//!          kind=6 Unrelate: [from u64][to u64][kind u16]
//! ```
//!
//! kind 1–3(`DeleteKey`/`DeleteRow`/`UpdateRow`)由设计保留:本层无需以 delta
//! 表示删除/更新(它们总以版本行承载),故编解码一律拒绝为 [`MnemeError::Corrupted`]。
//! 空区(旧段)解码为空 `Vec`。

use crate::core::error::{MnemeError, Result};
use crate::core::meta::{self, Meta};
use crate::persist::{
    Cursor, check_version, crc32, put_bytes_u32, put_i64, put_u16, put_u32, put_u64,
};

/// delta 区魔数。
pub(crate) const MAGIC: [u8; 4] = *b"DLT1";
/// delta 区格式版本。
pub(crate) const FORMAT_VERSION: u16 = 0x0001;
/// 区头长度:`magic(4) + ver(2) + count(2) + crc(4)`。
const HEADER_LEN: usize = 12;
/// 单条 delta 的公共前缀:`kind(1) + seqno(8) + tx_ms(8) + ns_id(4)`。
const COMMON_BYTES: usize = 21;

const KIND_ACCESS: u8 = 4;
const KIND_RELATE: u8 = 5;
const KIND_UNRELATE: u8 = 6;

/// 一条跨段覆盖条目(仅访问统计与关系边;删除/更新由版本行承载)。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DeltaEntry {
    /// 访问统计增量:恢复时 `access_count += access_delta`, `last_access_ms = at_ms`。
    Access {
        /// 全局写序号(排序/审计用)。
        seqno: u64,
        /// 事务时间(Unix 毫秒)。
        tx_ms: i64,
        /// 所属命名空间(审计用;应用不依赖)。
        ns_id: u32,
        /// 目标记录。
        rowid: u64,
        /// 最近访问时刻(Unix 毫秒)。
        last_access_ms: i64,
        /// 自上次物化以来的访问次数增量。
        access_delta: u32,
        /// 重要性提升增量(显式 `touch(boost)` 走版本行,本字段恒 0;格式保留)。
        importance_delta: f32,
    },
    /// 新增/更新关系边。
    Relate {
        /// 全局写序号。
        seqno: u64,
        /// 事务时间(Unix 毫秒)。
        tx_ms: i64,
        /// 起点所在命名空间(审计用)。
        ns_id: u32,
        /// 起点。
        from: u64,
        /// 终点。
        to: u64,
        /// 关系类型编号。
        kind: u16,
        /// 边权。
        weight: f32,
        /// 边元数据。
        meta: Meta,
    },
    /// 删除关系边。
    Unrelate {
        /// 全局写序号。
        seqno: u64,
        /// 事务时间(Unix 毫秒)。
        tx_ms: i64,
        /// 起点所在命名空间(审计用)。
        ns_id: u32,
        /// 起点。
        from: u64,
        /// 终点。
        to: u64,
        /// 关系类型编号。
        kind: u16,
    },
}

impl DeltaEntry {
    /// 公共前缀字段。
    fn common(&self) -> (u8, u64, i64, u32) {
        match self {
            DeltaEntry::Access {
                seqno,
                tx_ms,
                ns_id,
                ..
            } => (KIND_ACCESS, *seqno, *tx_ms, *ns_id),
            DeltaEntry::Relate {
                seqno,
                tx_ms,
                ns_id,
                ..
            } => (KIND_RELATE, *seqno, *tx_ms, *ns_id),
            DeltaEntry::Unrelate {
                seqno,
                tx_ms,
                ns_id,
                ..
            } => (KIND_UNRELATE, *seqno, *tx_ms, *ns_id),
        }
    }

    /// 排序目标(设计 04 §2.2a:按 `(target, seqno)` 排序)。
    fn target(&self) -> u64 {
        match self {
            DeltaEntry::Access { rowid, .. } => *rowid,
            DeltaEntry::Relate { from, .. } | DeltaEntry::Unrelate { from, .. } => *from,
        }
    }

    /// 条目种类编号(排序次级键)。
    fn kind_rank(&self) -> u8 {
        self.common().0
    }
}

/// 编码 delta 区(按 `(target, seqno, kind)` 排序;空条目返回空 `Vec`)。
///
/// # Errors
/// 条目数超过 `u16::MAX` 时返回 [`MnemeError::LimitExceeded`],绝不静默截断计数
/// (截断会产出解码端判为"尾部残留"的损坏段,FC-PERSIST-ERR-011)。
pub(crate) fn encode_delta(entries: &[DeltaEntry]) -> Result<Vec<u8>> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let mut sorted: Vec<&DeltaEntry> = entries.iter().collect();
    sorted.sort_by_key(|entry| (entry.target(), entry.common().1, entry.kind_rank()));
    let count = u16::try_from(sorted.len()).map_err(|_| MnemeError::LimitExceeded {
        field: "delta entries",
        limit: u16::MAX as usize,
        got: sorted.len(),
    })?;

    let mut body = Vec::new();
    for entry in &sorted {
        encode_entry(&mut body, entry);
    }

    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&MAGIC);
    put_u16(&mut out, FORMAT_VERSION);
    put_u16(&mut out, count);
    put_u32(&mut out, crc32(&body));
    out.extend_from_slice(&body);
    Ok(out)
}

/// 编码单条 delta 条目(公共前缀 + 按 kind 的载荷)。
fn encode_entry(out: &mut Vec<u8>, entry: &DeltaEntry) {
    let (kind, seqno, tx_ms, ns_id) = entry.common();
    out.push(kind);
    put_u64(out, seqno);
    put_i64(out, tx_ms);
    put_u32(out, ns_id);
    match entry {
        DeltaEntry::Access {
            rowid,
            last_access_ms,
            access_delta,
            importance_delta,
            ..
        } => {
            put_u64(out, *rowid);
            put_i64(out, *last_access_ms);
            put_u32(out, *access_delta);
            out.extend_from_slice(&importance_delta.to_le_bytes());
        }
        DeltaEntry::Relate {
            from,
            to,
            kind,
            weight,
            meta,
            ..
        } => {
            put_u64(out, *from);
            put_u64(out, *to);
            put_u16(out, *kind);
            out.extend_from_slice(&weight.to_le_bytes());
            put_bytes_u32(out, &meta::to_bytes(meta));
        }
        DeltaEntry::Unrelate { from, to, kind, .. } => {
            put_u64(out, *from);
            put_u64(out, *to);
            put_u16(out, *kind);
        }
    }
}

/// 解码 delta 区;空区返回空 `Vec`。
///
/// # Errors
/// 魔数/版本/CRC 不符、条目截断、未知 kind 或非法 meta 时返回
/// [`MnemeError::Corrupted`](FC-PERSIST-ERR-011)。
pub(crate) fn decode_delta(bytes: &[u8]) -> Result<Vec<DeltaEntry>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.len() < HEADER_LEN {
        return Err(corrupted("delta: 区短于头部"));
    }
    let mut cursor = Cursor::new(bytes, "msec delta");
    if cursor.take(4)? != MAGIC {
        return Err(corrupted("delta: 魔数不符"));
    }
    check_version("delta", cursor.u16()?)?;
    let count = cursor.u16()? as usize;
    let stored_crc = cursor.u32()?;
    let body = cursor.take(cursor.remaining())?;
    if crc32(body) != stored_crc {
        return Err(corrupted("delta: 条目区 CRC 不符"));
    }
    decode_entries(body, count)
}

/// 解码条目区并拒绝尾部残留。
fn decode_entries(body: &[u8], count: usize) -> Result<Vec<DeltaEntry>> {
    let mut cursor = Cursor::new(body, "msec delta 条目");
    let mut entries = Vec::with_capacity(count.min(body.len() / COMMON_BYTES));
    for _ in 0..count {
        entries.push(decode_entry(&mut cursor)?);
    }
    if !cursor.is_empty() {
        return Err(corrupted("delta: 条目区尾部有残留字节"));
    }
    Ok(entries)
}

/// 解码单条 delta 条目。
fn decode_entry(cursor: &mut Cursor<'_>) -> Result<DeltaEntry> {
    let kind = cursor.u8()?;
    let seqno = cursor.u64()?;
    let tx_ms = cursor.i64()?;
    let ns_id = cursor.u32()?;
    Ok(match kind {
        KIND_ACCESS => DeltaEntry::Access {
            seqno,
            tx_ms,
            ns_id,
            rowid: cursor.u64()?,
            last_access_ms: cursor.i64()?,
            access_delta: cursor.u32()?,
            importance_delta: f32::from_le_bytes(cursor.take(4)?.try_into().unwrap_or([0; 4])),
        },
        KIND_RELATE => {
            let from = cursor.u64()?;
            let to = cursor.u64()?;
            let kind = cursor.u16()?;
            let weight = f32::from_le_bytes(cursor.take(4)?.try_into().unwrap_or([0; 4]));
            let meta_len = cursor.u32()? as usize;
            let meta = meta::from_bytes(cursor.take(meta_len)?)?;
            DeltaEntry::Relate {
                seqno,
                tx_ms,
                ns_id,
                from,
                to,
                kind,
                weight,
                meta,
            }
        }
        KIND_UNRELATE => DeltaEntry::Unrelate {
            seqno,
            tx_ms,
            ns_id,
            from: cursor.u64()?,
            to: cursor.u64()?,
            kind: cursor.u16()?,
        },
        _ => return Err(corrupted("delta: 未知条目种类")),
    })
}

/// 构造 delta 区损坏错误。
fn corrupted(reason: &str) -> MnemeError {
    MnemeError::Corrupted {
        segment: None,
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::meta::json;

    fn sample() -> Vec<DeltaEntry> {
        vec![
            DeltaEntry::Unrelate {
                seqno: 9,
                tx_ms: 90,
                ns_id: 1,
                from: 3,
                to: 4,
                kind: 0,
            },
            DeltaEntry::Access {
                seqno: 5,
                tx_ms: 50,
                ns_id: 1,
                rowid: 3,
                last_access_ms: 55,
                access_delta: 2,
                importance_delta: 0.0,
            },
            DeltaEntry::Relate {
                seqno: 6,
                tx_ms: 60,
                ns_id: 2,
                from: 1,
                to: 3,
                kind: 7,
                weight: 0.25,
                meta: json!({"why": "test"}),
            },
        ]
    }

    /// FC-PERSIST-POST-010(往返一致 + 按 `(target, seqno)` 排序)
    #[test]
    fn delta_roundtrip_is_sorted_and_lossless() {
        let bytes = encode_delta(&sample()).expect("encode");
        let decoded = decode_delta(&bytes).expect("decode");
        // 排序键 (target, seqno):(1,6) Relate,(3,5) Access,(3,9) Unrelate。
        assert_eq!(decoded.len(), 3);
        assert!(matches!(decoded[0], DeltaEntry::Relate { from: 1, .. }));
        assert!(matches!(
            decoded[1],
            DeltaEntry::Access {
                rowid: 3,
                access_delta: 2,
                ..
            }
        ));
        assert!(matches!(decoded[2], DeltaEntry::Unrelate { from: 3, .. }));
        let reencoded = encode_delta(&decoded).expect("reencode");
        assert_eq!(bytes, reencoded, "解码后重编码必须逐字节一致");
    }

    /// FC-PERSIST-POST-010(空区兼容)
    #[test]
    fn empty_delta_is_valid() {
        assert!(decode_delta(&[]).expect("空区").is_empty());
        assert!(encode_delta(&[]).expect("空编码").is_empty());
    }

    /// FC-PERSIST-ERR-011(条目数超 `u16::MAX` → `LimitExceeded`,绝不静默截断计数)
    #[test]
    fn delta_count_overflow_is_rejected() {
        let entry = |rowid: u64| DeltaEntry::Access {
            seqno: 1,
            tx_ms: 1,
            ns_id: 0,
            rowid,
            last_access_ms: 1,
            access_delta: 1,
            importance_delta: 0.0,
        };
        let boundary: Vec<DeltaEntry> = (0..u16::MAX as u64).map(entry).collect();
        let bytes = encode_delta(&boundary).expect("u16::MAX 条必须可编码");
        assert_eq!(
            decode_delta(&bytes).expect("decode").len(),
            u16::MAX as usize
        );

        let overflow: Vec<DeltaEntry> = (0..u16::MAX as u64 + 1).map(entry).collect();
        assert!(matches!(
            encode_delta(&overflow),
            Err(MnemeError::LimitExceeded { .. })
        ));
    }

    /// FC-PERSIST-ERR-011(魔数/CRC/未知 kind/尾部残留/截断/长度越界 → Corrupted)
    #[test]
    fn malformed_delta_is_rejected() {
        let bytes = encode_delta(&sample()).expect("encode");
        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        assert!(matches!(
            decode_delta(&bad_magic),
            Err(MnemeError::Corrupted { .. })
        ));

        let mut bad_crc = bytes.clone();
        let last = bad_crc.len() - 1;
        bad_crc[last] ^= 0xFF;
        assert!(matches!(
            decode_delta(&bad_crc),
            Err(MnemeError::Corrupted { .. })
        ));

        let mut truncated = bytes.clone();
        truncated.truncate(HEADER_LEN + 1);
        assert!(matches!(
            decode_delta(&truncated),
            Err(MnemeError::Corrupted { .. })
        ));

        // 未知 kind:改写第一条公共前缀后重算 CRC。
        let mut unknown = bytes.clone();
        unknown[HEADER_LEN] = 3;
        let crc = crc32(&unknown[HEADER_LEN..]);
        unknown[8..12].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            decode_delta(&unknown),
            Err(MnemeError::Corrupted { .. })
        ));

        // 尾部残留:在合法条目区后追加一个字节并重算 CRC。
        let mut residual = bytes.clone();
        residual.push(0xAB);
        let crc = crc32(&residual[HEADER_LEN..]);
        residual[8..12].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            decode_delta(&residual),
            Err(MnemeError::Corrupted { .. })
        ));

        // 长度越界:Relate 的 meta 长度字段改成超大值(挑 kinds 里的 Relate 条目)。
        let relate_bytes = encode_delta(&[sample().remove(2)]).expect("单条 Relate");
        let mut oversized = relate_bytes.clone();
        let meta_len_offset = HEADER_LEN + COMMON_BYTES + 8 + 8 + 2 + 4;
        oversized[meta_len_offset..meta_len_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let crc = crc32(&oversized[HEADER_LEN..]);
        oversized[8..12].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            decode_delta(&oversized),
            Err(MnemeError::Corrupted { .. })
        ));

        // 任意字节不 panic。
        for len in 0..bytes.len() {
            let _ = decode_delta(&bytes[..len]);
        }
    }
}
