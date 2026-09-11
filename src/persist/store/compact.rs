//! size-tiered compaction:合并段写入与段集原子替换(设计 07 §4)。
//!
//! 选段与幸存版本筛选由 L5(§[`crate::life::compact`])完成,本模块只负责:
//! 写新段三件套 → 提交替换段组的 MANIFEST → 旧段入 `trash/`;WAL 不重置
//! (未落盘尾部仍由 WAL 承载)。任意步骤崩溃时新段是孤儿、旧 MANIFEST 完好(I10)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::memory::config::Config;
use crate::memory::ops::{CompactionControl, CompactionPlan};
use crate::memory::table::WriterState;
use crate::persist::flush::{self, SegmentBuildInput};
use crate::persist::manifest::{Manifest, NsEntry, SegmentEntry};
use crate::persist::storage::{self, SEGMENTS_DIR, hidx_name, msec_name, vsec_name};
use crate::persist::trash;
use crate::persist::{FORMAT_VERSION, crc32};

use super::Store;
use super::manifest_io;

impl Store {
    /// 执行一次 compaction:把 `plan` 组内段合并为只含 `keep_slots` 的新段。
    ///
    /// 提交成功后替换 MANIFEST 段集并把旧段移入 `trash/`;`control` 处于暂停
    /// 时在提交前中止(删除已写新段,活跃段集不变)并返回 `Ok(false)`。
    ///
    /// # Returns
    /// `Ok(true)` = 已提交;`Ok(false)` = 因暂停中止(调用方不得改动内存状态)。
    ///
    /// # Errors
    /// 只读模式返回 [`MnemeError::Unsupported`];段组与 MANIFEST 不符返回
    /// [`MnemeError::Inconsistent`];I/O/编码失败返回结构化错误(I10/FC-LIFE-ERR-001)。
    pub(crate) fn compact(
        &self,
        ws: &mut WriterState,
        config: &Config,
        plan: &CompactionPlan,
        keep_slots: &[usize],
        control: &CompactionControl,
    ) -> Result<bool> {
        if self.read_only {
            return Err(MnemeError::Unsupported {
                feature: "只读模式写入",
            });
        }
        let previous = self.manifest_snapshot();
        // 计划段必须在当前 MANIFEST 中且不重复(Builder 校验下正常不可达)。
        let mut group: Vec<SegmentEntry> = Vec::new();
        for id in &plan.segments {
            let Some(segment) = previous
                .segments
                .iter()
                .find(|segment| segment.segment_id == *id)
            else {
                return Err(MnemeError::Inconsistent {
                    reason: "compaction 计划包含非活跃段",
                });
            };
            group.push(*segment);
        }
        if group.is_empty() {
            return Err(MnemeError::Inconsistent {
                reason: "compaction 计划为空",
            });
        }

        let now_ms = config.clock.now_unix_ms();
        let segment_id = previous.next_segment_id;
        let encoded = flush::build_segment(
            ws,
            config,
            now_ms,
            &SegmentBuildInput {
                slots: keep_slots,
                delta: &[],
                full_relations: true,
            },
        )?;

        let names = [
            format!("{SEGMENTS_DIR}/{}", vsec_name(segment_id)),
            format!("{SEGMENTS_DIR}/{}", msec_name(segment_id)),
        ];
        self.write_file(&names[0], &encoded.vsec)?;
        self.write_file(&names[1], &encoded.msec)?;
        if let Some(hidx) = &encoded.hidx {
            self.write_file(&format!("{SEGMENTS_DIR}/{}", hidx_name(segment_id)), hidx)?;
        }
        control.mark_progress(0.5);
        if control.is_paused() {
            // 提交前中止:删除刚写的孤儿新段,活跃段集与数据不变。
            for name in &names {
                storage::remove_if_exists(&storage::resolve(&self.root, name)?)?;
            }
            let hidx = format!("{SEGMENTS_DIR}/{}", hidx_name(segment_id));
            storage::remove_if_exists(&storage::resolve(&self.root, &hidx)?)?;
            return Ok(false);
        }

        let new_manifest = next_manifest_after_merge(
            &previous,
            ws,
            self.dimension,
            self.metric,
            segment_id,
            keep_slots,
            &group,
            now_ms,
            &encoded,
        );
        manifest_io::commit_manifest(&self.root, &new_manifest, self.hook.as_deref())?;

        // 旧段移入 trash 并清理(读者持内存视图,不持段句柄)。
        let old_names: Vec<String> = plan
            .segments
            .iter()
            .flat_map(|id| [vsec_name(*id), msec_name(*id), hidx_name(*id)])
            .collect();
        trash::move_to_trash(&self.root, &old_names)?;
        trash::purge(&self.root)?;

        // 写状态登记新段并替换旧段索引;关系表已全量重写,清空关系 delta。
        Arc::make_mut(&mut ws.indexes).retain(|index| {
            index.segment_id != segment_id && !plan.segments.contains(&index.segment_id)
        });
        ws.install_segment(segment_id, keep_slots, encoded.index);
        ws.clear_edge_dirty();
        self.publish_manifest(&new_manifest);
        Ok(true)
    }
}

/// 构造"段组替换为新段"的 MANIFEST(watermark 不变,WAL 不重置)。
#[allow(clippy::too_many_arguments)]
fn next_manifest_after_merge(
    previous: &Manifest,
    ws: &WriterState,
    dimension: u32,
    metric: crate::core::metric::Metric,
    segment_id: u32,
    keep_slots: &[usize],
    group: &[SegmentEntry],
    created_ms: i64,
    encoded: &flush::EncodedSegment,
) -> Manifest {
    let mut segments: Vec<SegmentEntry> = previous
        .segments
        .iter()
        .filter(|segment| {
            !group
                .iter()
                .any(|held| held.segment_id == segment.segment_id)
        })
        .cloned()
        .collect();
    let (min_seqno, max_seqno) = seqno_range(ws, keep_slots);
    segments.push(SegmentEntry {
        segment_id,
        format_version: FORMAT_VERSION,
        row_count: keep_slots.len() as u64,
        min_seqno,
        max_seqno,
        created_ms,
        vsec_crc: crc32(&encoded.vsec),
        msec_crc: crc32(&encoded.msec),
        hidx_crc: encoded.hidx.as_deref().map_or(0, crc32),
        entry_slot: encoded.entry_slot,
        entry_level: encoded.entry_level,
    });
    let mut namespaces: Vec<NsEntry> = ws
        .ns_registry
        .iter()
        .map(|(id, path)| NsEntry {
            ns_id: id.get(),
            path: Arc::clone(path),
        })
        .collect();
    namespaces.sort_by_key(|entry| entry.ns_id);
    Manifest {
        dimension,
        metric,
        stopwords: previous.stopwords,
        next_rel_kind: previous.next_rel_kind,
        manifest_version: previous.manifest_version + 1,
        watermark_seqno: previous.watermark_seqno,
        next_rowid: ws.next_rowid,
        next_segment_id: segment_id + 1,
        next_ns_id: ws.next_ns_id,
        namespaces,
        rel_kinds: previous.rel_kinds.clone(),
        segments,
    }
}

/// 计算指定槽位的 `(min_seqno, max_seqno)`;空集合返回 `(0, 0)`。
fn seqno_range(ws: &WriterState, slot_indices: &[usize]) -> (u64, u64) {
    let mut min = u64::MAX;
    let mut max = 0_u64;
    for &index in slot_indices {
        let value = ws.slots[index].seqno.get();
        min = min.min(value);
        max = max.max(value);
    }
    if slot_indices.is_empty() {
        (0, 0)
    } else {
        (min, max)
    }
}
