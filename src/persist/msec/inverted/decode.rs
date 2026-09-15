//! 倒排区解码:词表、postings 区间校验同 doc 区装配。

use std::sync::Arc;

use crate::core::error::Result;
use crate::core::types::NsId;
use crate::memory::analysis::InvertedIndex;
use crate::persist::Cursor;

use super::super::index::{corrupted, read_utf8};
use super::postings::{decode_postings, remap_slot};

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
        if doc_len == 0 {
            // 空文本在编码端不入 doc 区;0 词长会令 BM25 `avgdl=0`,分数出现 NaN。
            return Err(corrupted("inverted: doc 区 doc_len 为 0"));
        }
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
