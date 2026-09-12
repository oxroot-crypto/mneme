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
use crate::persist::flush::{self, EncodedSegment, SegmentBuildInput};
use crate::persist::manifest::{Manifest, NsEntry, SegmentEntry};
use crate::persist::msec;
use crate::persist::storage::{self, SEGMENTS_DIR, hidx_name, msec_name, vsec_name};
use crate::persist::trash;
use crate::persist::{FORMAT_VERSION, crc32};

use super::Store;
use super::manifest_io;

/// 新段文件全部写完、MANIFEST 提交之前的进度。
const MERGE_PROGRESS_WRITTEN: f32 = 0.5;

/// [`Store::compact`] 的输入(参数收敛,rust 规范 §5)。
pub(crate) struct CompactInput<'a> {
    /// 本轮合并的段组计划。
    pub(crate) plan: &'a CompactionPlan,
    /// 写入新段的幸存槽位(升序)。
    pub(crate) keep_slots: &'a [usize],
    /// 后台合并控制句柄。
    pub(crate) control: &'a CompactionControl,
}

/// 一次合并的段组上下文(参数收敛)。
struct MergeContext<'a> {
    /// 提交前的 MANIFEST。
    previous: &'a Manifest,
    /// 被替换的旧段组。
    group: &'a [SegmentEntry],
    /// 合并时刻(Unix 毫秒)。
    now_ms: i64,
}

/// 已写完三件套、尚未提交的新段。
struct MergedSegment {
    /// 新段编号。
    segment_id: u32,
    /// 段字节与内存索引。
    encoded: EncodedSegment,
}

/// [`next_manifest_after_merge`] 的输入。
struct MergeManifestInput<'a> {
    /// 提交前的 MANIFEST。
    previous: &'a Manifest,
    /// 写状态(注册表与水位)。
    ws: &'a WriterState,
    /// 新段编号。
    segment_id: u32,
    /// 新段包含的幸存槽位。
    keep_slots: &'a [usize],
    /// 被替换的旧段组。
    group: &'a [SegmentEntry],
    /// 新段创建时刻(Unix 毫秒)。
    created_ms: i64,
    /// 新段编码产物。
    encoded: &'a EncodedSegment,
}

impl Store {
    /// 执行一次 compaction:把 `input.plan` 组内段合并为只含 `keep_slots` 的新段。
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
        input: &CompactInput<'_>,
    ) -> Result<bool> {
        if self.read_only {
            return Err(MnemeError::Unsupported {
                feature: "只读模式写入",
            });
        }
        let previous = self.manifest_snapshot();
        let group = resolve_group(&previous, input.plan)?;
        let context = MergeContext {
            previous: &previous,
            group: &group,
            now_ms: config.clock.now_unix_ms(),
        };
        let Some(merged) = self.build_merged(ws, config, &context, input)? else {
            return Ok(false);
        };
        let new_manifest = next_manifest_after_merge(&MergeManifestInput {
            previous: &previous,
            ws,
            segment_id: merged.segment_id,
            keep_slots: input.keep_slots,
            group: &group,
            created_ms: context.now_ms,
            encoded: &merged.encoded,
        });
        manifest_io::commit_manifest(&self.root, &new_manifest, self.hook.as_deref())?;
        self.install_merge(ws, input, &merged, &new_manifest);
        self.cleanup_old_segments(&input.plan.segments);
        Ok(true)
    }

    /// 写新段三件套;运行中暂停时删除孤儿并返回 `None`。
    fn build_merged(
        &self,
        ws: &WriterState,
        config: &Config,
        context: &MergeContext<'_>,
        input: &CompactInput<'_>,
    ) -> Result<Option<MergedSegment>> {
        let segment_id = context.previous.next_segment_id;
        let carried = self.carried_access_deltas(context.group, ws, input.keep_slots)?;
        let encoded = flush::build_segment(
            ws,
            config,
            context.now_ms,
            &SegmentBuildInput {
                slots: input.keep_slots,
                delta: &carried,
                full_relations: true,
            },
        )?;
        let names = segment_file_names(segment_id);
        self.write_merged_files(&names, &encoded)?;
        input.control.mark_progress(MERGE_PROGRESS_WRITTEN);
        if input.control.is_paused() {
            // 提交前中止:删除刚写的孤儿新段,活跃段集与数据不变。
            for name in &names {
                storage::remove_if_exists(&storage::resolve(&self.root, name)?)?;
            }
            return Ok(None);
        }
        Ok(Some(MergedSegment {
            segment_id,
            encoded,
        }))
    }

    /// 收集被合并段中尚未被新段覆盖的 `Access` delta。
    ///
    /// 新段版本行只覆盖 `keep_slots` 中的 RowId;其余 RowId 的访问增量必须随新段
    /// 继续承载,否则 compaction 后丢失(FC-PERSIST-POST-010)。关系变更不携带:
    /// 新段为全量关系表,已包含当前状态。
    fn carried_access_deltas(
        &self,
        group: &[SegmentEntry],
        ws: &WriterState,
        keep_slots: &[usize],
    ) -> Result<Vec<msec::DeltaEntry>> {
        let keep: std::collections::HashSet<usize> = keep_slots.iter().copied().collect();
        let mut carried = Vec::new();
        for segment in group {
            let rel = format!("{SEGMENTS_DIR}/{}", msec_name(segment.segment_id));
            let bytes = storage::read_file(&self.root, &rel)?;
            let view = msec::parse(&bytes)?;
            for entry in msec::decode_delta(view.delta_bytes())? {
                let msec::DeltaEntry::Access { rowid, .. } = entry else {
                    continue;
                };
                let Some(latest) = ws.latest.get(&crate::core::types::RowId::new(rowid)) else {
                    continue;
                };
                if keep.contains(&(latest.get() as usize)) {
                    continue;
                }
                carried.push(entry);
            }
        }
        Ok(carried)
    }

    /// 写入新段的 vsec/msec(以及可选的 hidx)文件。
    fn write_merged_files(&self, names: &[String; 3], encoded: &EncodedSegment) -> Result<()> {
        self.write_file(&names[0], &encoded.vsec)?;
        self.write_file(&names[1], &encoded.msec)?;
        if let Some(hidx) = &encoded.hidx {
            self.write_file(&names[2], hidx)?;
        }
        Ok(())
    }

    /// MANIFEST 提交后:对齐内存视图并尽力清理旧段。
    ///
    /// 清理失败不返回错误——旧段已不在活跃 MANIFEST 中,成为孤儿文件,由下次
    /// 启动清理;绝不留下"磁盘已换、内存未换"的半同步(I10/FC-LIFE-ERR-001)。
    fn install_merge(
        &self,
        ws: &mut WriterState,
        input: &CompactInput<'_>,
        merged: &MergedSegment,
        new_manifest: &Manifest,
    ) {
        Arc::make_mut(&mut ws.indexes).retain(|index| {
            index.segment_id != merged.segment_id
                && !input.plan.segments.contains(&index.segment_id)
        });
        ws.install_segment(
            merged.segment_id,
            input.keep_slots,
            merged.encoded.index.clone(),
            merged.encoded.quant,
            merged.encoded.recall_est,
        );
        ws.clear_edge_dirty();
        self.publish_manifest(new_manifest);
    }

    /// 把旧段移入 `trash/` 并清理;失败仅遗留孤儿文件。
    fn cleanup_old_segments(&self, segments: &[u32]) {
        let old_names: Vec<String> = segments
            .iter()
            .flat_map(|id| [vsec_name(*id), msec_name(*id), hidx_name(*id)])
            .collect();
        // reason: 提交已生效;移动/清理失败只遗留孤儿文件,不影响正确性与可读性。
        if trash::move_to_trash(&self.root, &old_names).is_ok() {
            // reason: purge 失败同样只遗留 trash 垃圾,不影响数据集正确性。
            let _ = trash::purge(&self.root);
        }
    }
}

/// 新段三件套的相对路径(顺序:`vsec`、`msec`、`hidx`)。
fn segment_file_names(segment_id: u32) -> [String; 3] {
    [
        format!("{SEGMENTS_DIR}/{}", vsec_name(segment_id)),
        format!("{SEGMENTS_DIR}/{}", msec_name(segment_id)),
        format!("{SEGMENTS_DIR}/{}", hidx_name(segment_id)),
    ]
}

/// 从当前 MANIFEST 解析计划段组;空计划或含非活跃段 → `Inconsistent`。
fn resolve_group(previous: &Manifest, plan: &CompactionPlan) -> Result<Vec<SegmentEntry>> {
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
    Ok(group)
}

/// 构造"段组替换为新段"的 MANIFEST(watermark 不变,WAL 不重置)。
fn next_manifest_after_merge(input: &MergeManifestInput<'_>) -> Manifest {
    let mut segments: Vec<SegmentEntry> = input
        .previous
        .segments
        .iter()
        .filter(|segment| {
            !input
                .group
                .iter()
                .any(|held| held.segment_id == segment.segment_id)
        })
        .cloned()
        .collect();
    segments.push(merged_segment_entry(input));
    let mut namespaces: Vec<NsEntry> = input
        .ws
        .ns_registry
        .iter()
        .map(|(id, path)| NsEntry {
            ns_id: id.get(),
            path: Arc::clone(path),
        })
        .collect();
    namespaces.sort_by_key(|entry| entry.ns_id);
    Manifest {
        dimension: input.previous.dimension,
        metric: input.previous.metric,
        stopwords: input.previous.stopwords,
        next_rel_kind: input.previous.next_rel_kind,
        manifest_version: input.previous.manifest_version + 1,
        watermark_seqno: input.previous.watermark_seqno,
        next_rowid: input.ws.next_rowid,
        next_segment_id: input.segment_id + 1,
        next_ns_id: input.ws.next_ns_id,
        namespaces,
        rel_kinds: input.previous.rel_kinds.clone(),
        segments,
    }
}

/// 新合并段的 MANIFEST 条目。
fn merged_segment_entry(input: &MergeManifestInput<'_>) -> SegmentEntry {
    let (min_seqno, max_seqno) = seqno_range(input.ws, input.keep_slots);
    SegmentEntry {
        segment_id: input.segment_id,
        format_version: FORMAT_VERSION,
        row_count: input.keep_slots.len() as u64,
        min_seqno,
        max_seqno,
        created_ms: input.created_ms,
        vsec_crc: crc32(&input.encoded.vsec),
        msec_crc: crc32(&input.encoded.msec),
        hidx_crc: input.encoded.hidx.as_deref().map_or(0, crc32),
        entry_slot: input.encoded.entry_slot,
        entry_level: input.encoded.entry_level,
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
