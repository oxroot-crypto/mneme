//! msec 倒排区编解码(`msec/inverted.rs`,设计 04 §2.2、§5)。
//!
//! 区布局:
//!
//! ```text
//! [u32 term_count]
//! 每词: [u32 ns_id][u32 term_len][term][u32 df]
//!       [u64 postings_offset(相对 postings 区起点)][u32 postings_len]
//! [u64 postings_total_len]
//! postings 区: 每词 [varint slot_delta][varint tf] × df(段内 SlotId 升序)
//! doc 区: [u32 doc_count] + 每文档 [u32 ns_id][u32 slot][u32 doc_len]
//! ```
//!
//! `postings_total_len` 显式给出 postings 区长度,doc 区起点不再靠"各词条
//! 声明区间的最大值"推断,畸形文件无法把 postings 读取引向 doc 区;
//! 所有长度/偏移均以 `checked` 运算校验,畸形文件返回 `Corrupted`,
//! 绝不 panic 或回绕。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::types::{NsId, SlotId};
use crate::core::varint;
use crate::memory::analysis::InvertedIndex;
use crate::persist::{Cursor, put_bytes_u32, put_u32, put_u64};

use super::index::{corrupted, read_utf8};

/// 编码倒排区(词表按 `(ns_id, term)` 排序,postings 差分编码)。
///
/// # Errors
/// postings 非 `SlotId` 升序 → `Inconsistent`;区长度/条目数超 `u32` → `LimitExceeded`/`TooLarge`。
pub(crate) fn encode_inverted(index: &InvertedIndex) -> Result<Vec<u8>> {
    let terms = sorted_terms(index);
    let mut dict = Vec::new();
    let mut postings_region = Vec::new();
    encode_term_dict(&terms, &mut dict, &mut postings_region)?;
    put_u64(&mut dict, postings_region.len() as u64);
    dict.extend_from_slice(&postings_region);
    encode_doc_region(index, &mut dict)?;
    Ok(dict)
}

/// 收集全部 `(ns_id, term)` 词条并按 `(ns_id, term)` 排序(保证编码确定性)。
fn sorted_terms(
    index: &InvertedIndex,
) -> Vec<(u32, Arc<str>, &[crate::memory::analysis::Posting])> {
    let mut terms: Vec<(u32, Arc<str>, &[crate::memory::analysis::Posting])> = Vec::new();
    for ns_id in index.ns_ids() {
        if let Some(bucket) = index.terms_of(ns_id) {
            for (term, postings) in bucket {
                terms.push((ns_id.get(), Arc::clone(term), postings.as_slice()));
            }
        }
    }
    terms.sort_by(|left, right| (left.0, left.1.as_ref()).cmp(&(right.0, right.1.as_ref())));
    terms
}

/// 编码词表与 postings 区(差分 `SlotId` + `tf`)。
fn encode_term_dict(
    terms: &[(u32, Arc<str>, &[crate::memory::analysis::Posting])],
    dict: &mut Vec<u8>,
    postings_region: &mut Vec<u8>,
) -> Result<()> {
    let term_count = u32::try_from(terms.len()).map_err(|_| MnemeError::TooLarge {
        field: "msec inverted 词表",
        limit: u32::MAX as usize,
        got: terms.len(),
    })?;
    put_u32(dict, term_count);
    for (ns_id, term, postings) in terms {
        let offset = postings_region.len() as u64;
        let mut previous: Option<u32> = None;
        for posting in *postings {
            let slot = posting.slot.get();
            if previous.is_some_and(|previous| slot <= previous) {
                // 差分编码要求严格升序;重复槽位由 len 隐含合并语义,拒绝静默编码。
                return Err(MnemeError::Inconsistent {
                    reason: "倒排 postings 未严格按 SlotId 升序",
                });
            }
            varint::encode_u32(slot - previous.unwrap_or(0), postings_region);
            varint::encode_u32(posting.tf, postings_region);
            previous = Some(slot);
        }
        let length = postings_region.len() as u64 - offset;
        put_u32(dict, *ns_id);
        put_bytes_u32(dict, term.as_bytes());
        let df = u32::try_from(postings.len()).map_err(|_| MnemeError::TooLarge {
            field: "msec inverted df",
            limit: u32::MAX as usize,
            got: postings.len(),
        })?;
        put_u32(dict, df);
        put_u64(dict, offset);
        let length = u32::try_from(length).map_err(|_| MnemeError::LimitExceeded {
            field: "msec inverted postings",
            limit: u32::MAX as usize,
            got: postings_region.len(),
        })?;
        put_u32(dict, length);
    }
    Ok(())
}

/// 编码 doc 区(每文档 `ns_id` / `slot` / 词数)。
fn encode_doc_region(index: &InvertedIndex, out: &mut Vec<u8>) -> Result<()> {
    let mut docs: Vec<(u32, u32, u32)> = Vec::new();
    for ns_id in index.ns_ids() {
        if let Some(entries) = index.doc_entries(ns_id) {
            for (slot, doc_len) in entries {
                docs.push((ns_id.get(), slot.get(), *doc_len));
            }
        }
    }
    docs.sort_unstable();
    let doc_count = u32::try_from(docs.len()).map_err(|_| MnemeError::TooLarge {
        field: "msec inverted doc 区",
        limit: u32::MAX as usize,
        got: docs.len(),
    })?;
    put_u32(out, doc_count);
    for (ns_id, slot, doc_len) in docs {
        put_u32(out, ns_id);
        put_u32(out, slot);
        put_u32(out, doc_len);
    }
    Ok(())
}

/// 解码倒排区,并把段内 `SlotId` 经 `remap` 映射为全局槽位。
///
/// # Errors
/// 结构非法、槽位越界、varint 畸形时返回 `Corrupted`。
pub(crate) fn decode_inverted(bytes: &[u8], remap: &[u32]) -> Result<InvertedIndex> {
    let mut cursor = Cursor::new(bytes, "msec inverted");
    let term_count = cursor.u32()? as usize;
    let entries = decode_term_entries(&mut cursor, term_count)?;
    // 显式 postings 区长度(词表之后、postings 数据之前)。
    let postings_len =
        usize::try_from(cursor.u64()?).map_err(|_| corrupted("inverted: 区长度溢出"))?;
    let postings_start = bytes.len() - cursor.remaining();
    let postings_end = postings_start
        .checked_add(postings_len)
        .ok_or_else(|| corrupted("inverted: postings 区偏移溢出"))?;
    let postings_region = bytes
        .get(postings_start..postings_end)
        .ok_or_else(|| corrupted("inverted: postings 区越界"))?;
    validate_postings_spans(&entries, postings_region.len())?;
    let mut index = build_text_index(&entries, postings_region, remap)?;
    decode_doc_region(bytes, postings_end, remap, &mut index)?;
    Ok(index)
}

/// 解码词表条目(逐条读 `ns_id`/term/df/offset/length);重复词条即拒绝。
fn decode_term_entries(cursor: &mut Cursor<'_>, term_count: usize) -> Result<Vec<TermEntry>> {
    let mut entries = Vec::new();
    let mut seen: std::collections::HashSet<(u32, Arc<str>)> = std::collections::HashSet::new();
    for _ in 0..term_count {
        let ns_id = cursor.u32()?;
        let term = read_utf8(cursor, "inverted term")?;
        if !seen.insert((ns_id, Arc::clone(&term))) {
            return Err(corrupted("inverted: 词表存在重复词条"));
        }
        let df = cursor.u32()?;
        let offset = cursor.u64()?;
        let length = cursor.u32()?;
        entries.push(TermEntry {
            ns_id,
            term,
            df,
            offset,
            length,
        });
    }
    Ok(entries)
}

/// 校验各词条声明的区间不越出显式 postings 区范围;溢出/越界即拒绝。
fn validate_postings_spans(entries: &[TermEntry], region_len: usize) -> Result<()> {
    for entry in entries {
        let start = usize::try_from(entry.offset).map_err(|_| corrupted("inverted: 偏移溢出"))?;
        let end = start
            .checked_add(entry.length as usize)
            .ok_or_else(|| corrupted("inverted: 偏移溢出"))?;
        if end > region_len {
            return Err(corrupted("inverted: postings 区间超出区范围"));
        }
    }
    Ok(())
}

/// 解码全部词条的 postings 并装入倒排。
fn build_text_index(
    entries: &[TermEntry],
    postings_region: &[u8],
    remap: &[u32],
) -> Result<InvertedIndex> {
    let mut index = InvertedIndex::default();
    for entry in entries {
        let start = usize::try_from(entry.offset).map_err(|_| corrupted("inverted: 偏移溢出"))?;
        let end = start
            .checked_add(entry.length as usize)
            .ok_or_else(|| corrupted("inverted: 区间溢出"))?;
        let slice = postings_region
            .get(start..end)
            .ok_or_else(|| corrupted("inverted: postings 越界"))?;
        let postings = decode_postings(slice, entry.df, remap)?;
        index.insert_term(NsId::new(entry.ns_id), Arc::clone(&entry.term), postings);
    }
    Ok(index)
}

/// 解码 doc 区(每文档 `ns_id` / 段内槽位 / 词数)并装入倒排。
fn decode_doc_region(
    bytes: &[u8],
    doc_start: usize,
    remap: &[u32],
    index: &mut InvertedIndex,
) -> Result<()> {
    let doc_bytes = bytes
        .get(doc_start..)
        .ok_or_else(|| corrupted("inverted: doc 区越界"))?;
    let mut doc_cursor = Cursor::new(doc_bytes, "msec inverted docs");
    let doc_count = doc_cursor.u32()? as usize;
    for _ in 0..doc_count {
        let ns_id = NsId::new(doc_cursor.u32()?);
        let raw_slot = doc_cursor.u32()?;
        let doc_len = doc_cursor.u32()?;
        let slot = remap_slot(raw_slot, remap)?;
        index.insert_doc(ns_id, slot, doc_len);
    }
    if !doc_cursor.is_empty() {
        return Err(corrupted("inverted: doc 区尾部有残留字节"));
    }
    Ok(())
}

/// 词表条目。
struct TermEntry {
    ns_id: u32,
    term: Arc<str>,
    df: u32,
    offset: u64,
    length: u32,
}

/// 解码一条 postings:差分 SlotId 还原、映射全局槽位并合并同槽位。
fn decode_postings(
    slice: &[u8],
    df: u32,
    remap: &[u32],
) -> Result<Vec<crate::memory::analysis::Posting>> {
    let mut cursor = Cursor::new(slice, "msec postings");
    let mut slot = 0_u32;
    let mut postings = Vec::new();
    for _ in 0..df {
        let (delta, used) = varint::decode_u32(cursor.rest())?;
        cursor.advance(used)?;
        slot = slot
            .checked_add(delta)
            .ok_or_else(|| corrupted("postings: 槽位差溢出"))?;
        let (tf, used) = varint::decode_u32(cursor.rest())?;
        cursor.advance(used)?;
        postings.push(crate::memory::analysis::Posting {
            slot: remap_slot(slot, remap)?,
            tf,
        });
    }
    if !cursor.is_empty() {
        return Err(corrupted("postings: 尾部有残留字节"));
    }
    // 段内 SlotId 升序经重排映射后可能乱序;排序并合并同槽位。
    postings.sort_by_key(|posting| posting.slot);
    let mut merged: Vec<crate::memory::analysis::Posting> = Vec::with_capacity(postings.len());
    for posting in postings {
        match merged.last_mut() {
            Some(last) if last.slot == posting.slot => {
                last.tf = last
                    .tf
                    .checked_add(posting.tf)
                    .ok_or_else(|| corrupted("postings: 词频合并溢出"))?;
            }
            _ => merged.push(posting),
        }
    }
    Ok(merged)
}

/// 段内槽位 → 全局槽位;越界返回 `Corrupted`。
fn remap_slot(slot: u32, remap: &[u32]) -> Result<SlotId> {
    remap
        .get(slot as usize)
        .copied()
        .map(SlotId::new)
        .ok_or_else(|| corrupted("inverted: 槽位超出重排映射"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// FC-PERSIST-POST-008(倒排往返:经重排映射还原为全局槽位)
    #[test]
    fn inverted_roundtrip_with_remap() {
        let mut index = InvertedIndex::default();
        let ns = NsId::new(1);
        index.insert_text(SlotId::new(0), ns, "alpha beta", false);
        index.insert_text(SlotId::new(1), ns, "beta gamma delta", false);
        let bytes = encode_inverted(&index).expect("encode");
        // 段内槽位 0/1 重排为全局槽位 1/0。
        let remap = [1_u32, 0_u32];
        let decoded = decode_inverted(&bytes, &remap).expect("decode");
        let beta = decoded.postings_of(ns, "beta").expect("beta");
        assert_eq!(beta.len(), 2);
        assert_eq!(beta[0].slot, SlotId::new(0));
        assert_eq!(beta[1].slot, SlotId::new(1));
        let docs = decoded.doc_entries(ns).expect("docs");
        assert_eq!(docs.get(&SlotId::new(0)), Some(&3));
        assert_eq!(docs.get(&SlotId::new(1)), Some(&2));
    }

    #[test]
    fn postings_are_sorted_after_remap() {
        let postings = decode_postings(
            &{
                let mut bytes = Vec::new();
                varint::encode_u32(0, &mut bytes);
                varint::encode_u32(2, &mut bytes);
                varint::encode_u32(1, &mut bytes);
                varint::encode_u32(3, &mut bytes);
                bytes
            },
            2,
            &[5, 3],
        )
        .expect("decode");
        assert_eq!(postings[0].slot, SlotId::new(3));
        assert_eq!(postings[0].tf, 3);
        assert_eq!(postings[1].slot, SlotId::new(5));
        assert_eq!(postings[1].tf, 2);
    }

    /// FC-PERSIST-ERR-010(畸形倒排区 → `Corrupted`,不 panic)
    #[test]
    fn malformed_inverted_is_rejected_without_panic() {
        assert!(decode_inverted(&[0xFF; 4], &[]).is_err());
        // 声明 offset/length 指向显式 postings 区外,必须被拒绝而非回绕。
        let mut bytes = Vec::new();
        put_u32(&mut bytes, 1);
        put_u32(&mut bytes, 1);
        put_bytes_u32(&mut bytes, b"t");
        put_u32(&mut bytes, 1);
        put_u64(&mut bytes, u64::MAX);
        put_u32(&mut bytes, u32::MAX);
        put_u64(&mut bytes, 0);
        assert!(matches!(
            decode_inverted(&bytes, &[0]),
            Err(MnemeError::Corrupted { .. })
        ));
        // 词频合并溢出必须报错。
        let mut postings = Vec::new();
        varint::encode_u32(0, &mut postings);
        varint::encode_u32(u32::MAX, &mut postings);
        varint::encode_u32(0, &mut postings);
        varint::encode_u32(u32::MAX, &mut postings);
        assert!(decode_postings(&postings, 2, &[7]).is_err());

        // 区尾残留:合法编码后多 1 字节必须拒绝。
        let mut index = InvertedIndex::default();
        index.insert_text(SlotId::new(0), NsId::new(1), "alpha", false);
        let mut bytes = encode_inverted(&index).expect("encode");
        assert!(decode_inverted(&bytes, &[0]).is_ok());
        bytes.push(0);
        assert!(matches!(
            decode_inverted(&bytes, &[0]),
            Err(MnemeError::Corrupted { .. })
        ));

        // 词条重复:同 `(ns_id, term)` 出现两次必须拒绝。
        let mut bytes = Vec::new();
        put_u32(&mut bytes, 2);
        for _ in 0..2 {
            put_u32(&mut bytes, 1);
            put_bytes_u32(&mut bytes, b"t");
            put_u32(&mut bytes, 0);
            put_u64(&mut bytes, 0);
            put_u32(&mut bytes, 0);
        }
        put_u64(&mut bytes, 0);
        put_u32(&mut bytes, 0);
        assert!(matches!(
            decode_inverted(&bytes, &[]),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    /// FC-PERSIST-ERR-010(任意字节 + 任意 remap 不 panic)
    #[test]
    fn decode_inverted_never_panics_on_arbitrary_bytes() {
        proptest!(|(
            bytes in prop::collection::vec(any::<u8>(), 0..256),
            remap in prop::collection::vec(any::<u32>(), 0..16),
        )| {
            // 只证不 panic:decode 成败与结构合法性均不作断言。
            let _ = decode_inverted(&bytes, &remap);
        });
    }
}
