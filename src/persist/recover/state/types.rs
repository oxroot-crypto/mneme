//! 恢复输入/产物同段视图个数据结构。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::memory::lazy::ByteSource;
use crate::persist::msec;
use crate::persist::source::{ByteFile, SegmentHandle};
use crate::persist::vsec;

/// 一个待恢复的段:段号与三文件句柄(向量/邻接按需读,FC-PERSIST-INV-021)。
pub(crate) struct SegmentBytes {
    /// 段编号。
    pub(crate) segment_id: u32,
    /// 向量段文件句柄。
    pub(crate) vsec: Arc<ByteFile>,
    /// 元数据段文件句柄。
    pub(crate) msec: Arc<ByteFile>,
    /// HNSW 图文件句柄(`hidx_crc == 0` 时为 `None`)。
    pub(crate) hidx: Option<Arc<ByteFile>>,
}

impl SegmentBytes {
    /// 由段句柄构造恢复输入。
    pub(crate) fn from_handle(handle: &SegmentHandle) -> Self {
        Self {
            segment_id: handle.segment_id,
            vsec: Arc::clone(&handle.vsec),
            msec: Arc::clone(&handle.msec),
            hidx: handle.hidx.as_ref().map(Arc::clone),
        }
    }

    /// vsec 整段切片(句柄存活期内有效)。
    pub(crate) fn vsec_bytes(&self) -> Result<&[u8]> {
        self.vsec
            .slice_at(0, self.vsec.len())
            .ok_or_else(|| MnemeError::Corrupted {
                segment: Some(crate::core::types::SegmentId::new(self.segment_id)),
                reason: "vsec: 句柄切片失败".to_string(),
            })
    }

    /// msec 整段切片(句柄存活期内有效)。
    pub(crate) fn msec_bytes(&self) -> Result<&[u8]> {
        self.msec
            .slice_at(0, self.msec.len())
            .ok_or_else(|| MnemeError::Corrupted {
                segment: Some(crate::core::types::SegmentId::new(self.segment_id)),
                reason: "msec: 句柄切片失败".to_string(),
            })
    }
}

/// 一个已解析段及其"段内槽位 → 全局槽位"重排映射。
pub(crate) struct SegmentRemap {
    /// 段编号。
    pub(crate) segment_id: u32,
    /// 段内槽位 → 全局槽位(`remap[local] = global`)。
    pub(crate) remap: Vec<u32>,
}

/// 一个已解析段的视图与向量文件句柄(惰性向量构造用)。
pub(in crate::persist::recover) struct ParsedSegment<'a> {
    /// vsec 只读视图(借用段文件切片)。
    pub(in crate::persist::recover) vsec_view: vsec::VsecView<'a>,
    /// msec 只读视图(借用段文件切片)。
    pub(in crate::persist::recover) msec_view: msec::MsecView<'a>,
    /// 向量文件句柄(惰性向量长期持有,保证文件不提前回收)。
    pub(in crate::persist::recover) vsec_file: Arc<ByteFile>,
}

/// [`load_segments`](super::load::load_segments) 的恢复结果。
pub(crate) struct RecoveredSegments {
    /// 被隔离(跳过)的段 id 列表。
    pub(crate) skipped: Vec<u32>,
    /// 每个已解析段的重排映射(倒排载入与 hidx 载入共用)。
    pub(crate) remaps: Vec<SegmentRemap>,
}
