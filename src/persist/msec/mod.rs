//! 元数据段(`msec`)编解码(设计 04 §2.2、§5.5)。
//!
//! 文件 = 定长头(160 B 字段 + CRC,补齐到 192 B)+ 变长数据区 + 尾部 payload CRC。
//! 头部用 9 组 `offset/len` 指向各数据区;`doc_region`(记录体)、
//! `version_table`(版本链)、`key_index`、`ns_stats`、`delta`、`relations` 与
//! L4 的四类轻量索引区(`field_dict`/`zone_maps`/`blooms`/`inverted`)。
//!
//! **记录体(entry)**:`[u32 total_len][u64 rowid][u64 seqno][u32 ns_id][u8 flags]`
//! 之后按 flags 依次出现可选的 key/text、meta、时间与统计字段。墓碑版本无记录体,
//! 其 `doc_offset` 记为 [`TOMBSTONE_DOC_OFFSET`](`u64::MAX`),由 vsec 删除位图标为不可见。
//!
//! # 子模块
//!
//! * `encode` —— 段与记录体编码。
//! * `decode` —— 段头解析、记录体解码与只读视图 [`MsecView`]。
//! * `index` —— 字段字典 / zone map / bloom 三类轻量索引区编解码。
//! * `inverted` —— 倒排区编解码(含槽位重排映射)。

use std::sync::Arc;

use crate::core::meta::Meta;
use crate::core::types::{Key, NsId, RowId, SeqNo};

mod decode;
mod encode;
mod entry;
mod index;
mod inverted;

pub(crate) use decode::{MsecView, parse};
pub(crate) use encode::{encode, encode_entry};
pub(crate) use entry::entry_from_prefix;
pub(crate) use index::{
    FieldKind, decode_bloom, decode_field_dict, encode_bloom, encode_field_dict, encode_zmap,
    validate_zmap,
};
pub(crate) use inverted::{decode_inverted, encode_inverted};

/// 元数据段魔数。
pub(crate) const MAGIC: [u8; 4] = *b"MSC1";
/// 定长头部长度(字节):160 B 字段 + 4 B CRC,补齐到 64B 对齐。
pub(crate) const HEADER_LEN: u16 = 192;
/// 头部 CRC 覆盖的字节数(CRC 字段之前)。
const HEADER_CRC_COVER: usize = 160;
/// 墓碑版本在 `version_table` 中的 `doc_offset` 哨兵(无记录体)。
pub(crate) const TOMBSTONE_DOC_OFFSET: u64 = u64::MAX;
/// `version_table` 单行定长字节数。
const VERSION_ROW_BYTES: usize = 36;
/// `ns_stats` 单行定长字节数。
const NS_STAT_ROW_BYTES: usize = 20;
/// 各数据区起点的对齐字节数。
const REGION_ALIGN: usize = 8;

const FLAG_KEY: u8 = 1 << 0;
const FLAG_TEXT: u8 = 1 << 1;
const FLAG_TTL: u8 = 1 << 2;
const FLAG_IMPORTANCE: u8 = 1 << 3;
const FLAG_ACCESS: u8 = 1 << 4;
const FLAG_VALID_TIME: u8 = 1 << 5;
const FLAG_CONFIDENCE: u8 = 1 << 6;
const FLAG_PROVENANCE: u8 = 1 << 7;

/// 一条记录的元数据(不含向量;向量在 vsec)。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EntryData {
    /// 稳定逻辑标识。
    pub(crate) rowid: RowId,
    /// 全局写序号。
    pub(crate) seqno: SeqNo,
    /// 命名空间编号。
    pub(crate) ns_id: NsId,
    /// 可选业务 key。
    pub(crate) key: Option<Key>,
    /// 可选文本。
    pub(crate) text: Option<Arc<str>>,
    /// 元数据 JSON。
    pub(crate) meta: Meta,
    /// 事务时间(Unix 毫秒)。
    pub(crate) created_at_ms: i64,
    /// 逻辑过期时间(可选)。
    pub(crate) expires_at_ms: Option<i64>,
    /// 重要度(可选,缺省 0.5)。
    pub(crate) importance: Option<f32>,
    /// 访问统计 `(last_access_ms, access_count)`(可选)。
    pub(crate) access: Option<(i64, u32)>,
    /// 有效时间 `(valid_from_ms, valid_to_ms)`(可选)。
    pub(crate) valid_time: Option<(i64, Option<i64>)>,
    /// 可信度(可选,缺省 1.0)。
    pub(crate) confidence: Option<f32>,
    /// 来源/派生链(可选)。
    pub(crate) provenance: Option<Meta>,
}

/// 段内一个物理槽位:版本元信息 + 可选记录体(`None` = 墓碑)。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SlotMeta {
    /// 稳定逻辑标识。
    pub(crate) rowid: RowId,
    /// 全局写序号。
    pub(crate) seqno: SeqNo,
    /// 事务时间(Unix 毫秒)。
    pub(crate) tx_ms: i64,
    /// 记录体;墓碑为 `None`。
    pub(crate) body: Option<EntryData>,
}

/// `version_table` 行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VersionRow {
    /// 稳定逻辑标识。
    pub(crate) rowid: u64,
    /// 全局写序号。
    pub(crate) seqno: u64,
    /// 事务时间(Unix 毫秒)。
    pub(crate) tx_ms: i64,
    /// 物理槽位下标。
    pub(crate) slot_id: u32,
    /// 记录体在 `doc_region` 中的字节偏移;墓碑为 [`TOMBSTONE_DOC_OFFSET`]。
    pub(crate) doc_offset: u64,
}

/// `key_index` 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyIndexRow {
    /// 命名空间编号。
    pub(crate) ns_id: u32,
    /// 业务 key。
    pub(crate) key: Arc<str>,
    /// 稳定逻辑标识。
    pub(crate) rowid: u64,
    /// 物理槽位下标。
    pub(crate) slot_id: u32,
    /// 全局写序号。
    pub(crate) seqno: u64,
    /// 记录体在 `doc_region` 中的字节偏移。
    pub(crate) doc_offset: u64,
}

/// 命名空间级统计行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NsStatRow {
    /// 命名空间编号。
    pub(crate) ns_id: u32,
    /// 活行数。
    pub(crate) doc_count: u64,
    /// 文本总长度。
    pub(crate) total_doc_len: u64,
}

/// msec 编码输入。
pub(crate) struct MsecInput<'a> {
    /// 段内物理槽位(顺序即 `SlotId`,墓碑 `body = None`)。
    pub(crate) slots: &'a [SlotMeta],
    /// 命名空间统计。
    pub(crate) ns_stats: &'a [NsStatRow],
    /// 预编码的 delta(跨段覆盖)区;L2 全量快照恒为 `&[]`,该区保留给 L5 compaction。
    pub(crate) delta: &'a [u8],
    /// 预编码的 relations 区(见 [`crate::persist::edges`]);无边时为 `&[]`。
    pub(crate) relations: &'a [u8],
    /// 字段字典区(L4;无索引字段时为空)。
    pub(crate) field_dict: &'a [u8],
    /// zone map 区(L4;无索引字段时为空)。
    pub(crate) zmap: &'a [u8],
    /// bloom 区(L4;无 `key` 字段时为空)。
    pub(crate) bloom: &'a [u8],
    /// 倒排区(L4;无文本记录时为空)。
    pub(crate) inverted: &'a [u8],
}

/// msec 各数据区偏移/长度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Region {
    offset: u64,
    len: u64,
}

/// 头部 9 个数据区的偏移/长度(field_dict/version/key/inv/ns_stats/zmap/bloom/delta/rel)。
#[derive(Debug, Clone, Copy, Default)]
struct Regions {
    field_dict: Region,
    version: Region,
    key: Region,
    inv: Region,
    ns_stats: Region,
    zmap: Region,
    bloom: Region,
    delta: Region,
    rel: Region,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::MnemeError;
    use crate::core::meta::json;

    fn body(rowid: u64, seqno: u64, ns: u32, key: Option<&str>) -> EntryData {
        EntryData {
            rowid: RowId::new(rowid),
            seqno: SeqNo::new(seqno),
            ns_id: NsId::new(ns),
            key: key.map(Key::new),
            text: Some(Arc::from("hello world")),
            meta: json!({"kind": "note"}),
            created_at_ms: 1_700_000_000_000,
            expires_at_ms: Some(1_800_000_000_000),
            importance: Some(0.7),
            access: Some((1_700_000_000_100, 3)),
            valid_time: Some((1_000, Some(2_000))),
            confidence: Some(0.9),
            provenance: Some(json!({"source": "user"})),
        }
    }

    fn sample_slots() -> Vec<SlotMeta> {
        vec![
            SlotMeta {
                rowid: RowId::new(0),
                seqno: SeqNo::new(1),
                tx_ms: 10,
                body: Some(body(0, 1, 1, Some("alpha"))),
            },
            SlotMeta {
                rowid: RowId::new(0),
                seqno: SeqNo::new(2),
                tx_ms: 20,
                body: None,
            },
            SlotMeta {
                rowid: RowId::new(1),
                seqno: SeqNo::new(3),
                tx_ms: 30,
                body: Some(body(1, 3, 2, None)),
            },
        ]
    }

    fn encode_sample() -> Vec<u8> {
        let slots = sample_slots();
        let ns_stats = [NsStatRow {
            ns_id: 1,
            doc_count: 1,
            total_doc_len: 11,
        }];
        encode(&MsecInput {
            slots: &slots,
            ns_stats: &ns_stats,
            delta: &[],
            relations: &[],
            field_dict: &[],
            zmap: &[],
            bloom: &[],
            inverted: &[],
        })
        .expect("encode")
    }

    /// 记录体字段往返一致。
    #[test]
    fn msec_entry_roundtrip() {
        let entry = body(7, 9, 3, Some("k"));
        let bytes = encode_entry(&entry).expect("encode");
        assert_eq!(entry_from_prefix(&bytes).expect("decode"), entry);
    }

    /// 段级往返:版本链、key 索引、墓碑 `doc_offset` 与统计一致。
    #[test]
    fn msec_segment_roundtrip() {
        let slots = sample_slots();
        let bytes = encode_sample();
        let mut view = parse(&bytes).expect("parse");
        assert_eq!(view.row_count(), 3);
        let versions = view.version_rows().expect("versions");
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].slot_id, 0);
        assert_eq!(versions[1].slot_id, 1);
        assert_eq!(versions[1].doc_offset, TOMBSTONE_DOC_OFFSET);
        assert_eq!(versions[2].slot_id, 2);

        let keys = view.key_rows().expect("keys");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key.as_ref(), "alpha");
        assert_eq!(keys[0].ns_id, 1);

        let stats = view.ns_stat_rows().expect("stats");
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].ns_id, 1);

        let first = view.read_entry(versions[0].doc_offset).expect("read");
        assert_eq!(first.as_ref(), slots[0].body.as_ref());
        assert!(
            view.read_entry(TOMBSTONE_DOC_OFFSET)
                .expect("tomb")
                .is_none()
        );
        view.verify_payload().expect("payload crc");
    }

    /// 头部损坏必须被检出。
    #[test]
    fn msec_detects_header_corruption() {
        let mut bytes = encode_sample();
        bytes[8] ^= 0xFF;
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }

    /// 魔数不符 → `Corrupted`。
    #[test]
    fn msec_rejects_bad_magic() {
        let mut bytes = encode_sample();
        bytes[0] = b'X';
        assert!(matches!(parse(&bytes), Err(MnemeError::Corrupted { .. })));
    }
}
