//! 倒排区编码:词表、postings 同 doc 区。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::varint;
use crate::memory::analysis::InvertedIndex;
use crate::persist::{put_bytes_u32, put_u32, put_u64};

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
        for (term, postings) in index.terms_of(ns_id) {
            terms.push((ns_id.get(), Arc::clone(term), postings));
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
        for (slot, doc_len) in index.docs_of(ns_id) {
            docs.push((ns_id.get(), slot.get(), *doc_len));
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
