//! 段内 postings 解码:差分槽位还原、重映射同同槽位词频合并。

use crate::core::error::Result;
use crate::core::types::SlotId;
use crate::core::varint;
use crate::persist::Cursor;

use super::super::index::corrupted;

/// 解码一条 postings:差分 SlotId 还原、映射全局槽位并合并同槽位。
pub(super) fn decode_postings(
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
pub(super) fn remap_slot(slot: u32, remap: &[u32]) -> Result<SlotId> {
    remap
        .get(slot as usize)
        .copied()
        .map(SlotId::new)
        .ok_or_else(|| corrupted("inverted: 槽位超出重排映射"))
}
