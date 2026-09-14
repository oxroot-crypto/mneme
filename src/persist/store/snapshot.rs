//! 增量段 flush 与备份(`store/snapshot.rs`)。
//!
//! `flush` 只把未落盘槽位与跨段 delta 物化为一个新段、提交新 MANIFEST 并
//! Checkpoint WAL(重置);已提交旧段保持活跃、write-once、不入 `trash/`。
//! `backup_to` 复制全部文件到目标目录,最后写 `current` 保证备份原子可用。

use std::path::Path;
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::memory::config::Config;
use crate::memory::table::WriterState;
use crate::persist::flush::{self, SegmentBuildInput};
use crate::persist::manifest::{Manifest, NsEntry, RelKindEntry, SegmentEntry};
use crate::persist::storage::{
    self, CURRENT_FILE, SEGMENTS_DIR, WAL_DIR, hidx_name, manifest_name, msec_name, vsec_name,
};
use crate::persist::{FORMAT_VERSION, crc32};

use super::Store;
use super::manifest_io;

/// 备份复制目标与统计(参数收敛);硬链接失败即把 `hardlinked` 置 `false`。
struct CopySink<'a> {
    /// 源库根目录。
    root: &'a Path,
    /// 备份目标目录。
    target: &'a Path,
    /// 已复制文件数与字节数。
    counts: CopyCounts,
    /// 是否全部走硬链接。
    hardlinked: bool,
}

impl CopySink<'_> {
    /// 同盘优先硬链接一个必存段文件,失败时回退逐字节复制。
    fn link_or_copy_required(&mut self, rel: &str) -> Result<()> {
        let source = storage::resolve(self.root, rel)?;
        let destination = storage::resolve(self.target, rel)?;
        if std::fs::hard_link(&source, &destination).is_ok() {
            self.counts.files += 1;
            // reason: 统计为尽力而为;元数据读取失败仅少计字节,不影响备份正确性。
            self.counts.bytes += std::fs::metadata(&source).map_or(0, |metadata| metadata.len());
            return Ok(());
        }
        self.hardlinked = false;
        self.copy_required(rel)
    }

    /// 复制一个必须存在的库内文件;缺失返回 [`MnemeError::Corrupted`]。
    fn copy_required(&mut self, rel: &str) -> Result<()> {
        // 被 MANIFEST 引用的段必须存在;缺失即备份不可信,绝不静默产出残档。
        let content =
            storage::read_file(self.root, rel).map_err(|error| MnemeError::Corrupted {
                segment: None,
                reason: format!("备份:必存文件缺失或不可读:{rel}: {error}"),
            })?;
        self.counts.files += 1;
        self.counts.bytes += content.len() as u64;
        storage::write_atomic(self.target, rel, &content)
    }
}

/// 大 flush 切块行数:环境变量 `MNEME_FLUSH_CHUNK_ROWS` 优先,否则取
/// `Tuning.flush_chunk_rows`(默认 65_536;非法/0 值回退配置值)。
///
/// 小于该规模保持「一次 flush = 一段」语义;大规模建库按此切块,块数 =
/// ⌈行数/本值⌉(每块一个段),并行度只限制**同时**构建的块数。
pub(super) fn flush_chunk_rows(tuning: &crate::core::options::Tuning) -> usize {
    std::env::var("MNEME_FLUSH_CHUNK_ROWS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|rows| *rows > 0)
        .unwrap_or(tuning.flush_chunk_rows.max(1))
}

/// 并行构建的内存预算(字节):单个构建块的编码缓冲约占
/// `块行数 × (维度 × 4 + 256) × 2`(vsec f32 区 + msec/hidx/量化副本与临时
/// 缓冲)。超过预算时降低同时构建的块数,避免大维度多核并行触发换页(swap)。
const FLUSH_BUILD_MEMORY_BUDGET: u64 = 6 * 1024 * 1024 * 1024;

/// 块级并行构建度:环境变量 `MNEME_FLUSH_THREADS` 优先,否则取
/// `Tuning.flush_threads`(默认 1 = 块级串行,由块内批并行承担加速;
/// 实测 4 核机上嵌套并行因带宽争抢反而更慢)。
pub(super) fn flush_parallelism(tuning: &crate::core::options::Tuning) -> usize {
    if let Ok(value) = std::env::var("MNEME_FLUSH_THREADS") {
        // reason: 调参入口;非法值按串行处理,只影响性能不影响正确性。
        return value.parse::<usize>().unwrap_or(1).max(1);
    }
    tuning.flush_threads.max(1)
}

/// 并行构建的块数:受并行度、块数与内存预算三者约束。
///
/// `dimension`/`chunk_rows` 用于估算单块编码缓冲;预算不足时宁可少并行,
/// 也不让多个大块同时驻留把机器拖进 swap(内存带宽已使并行收益递减)。
fn flush_build_threads(
    dimension: usize,
    chunk_rows: usize,
    parallelism: usize,
    chunk_count: usize,
) -> usize {
    let per_block = (chunk_rows as u64)
        .saturating_mul((dimension as u64).saturating_mul(4).saturating_add(256))
        .saturating_mul(2)
        .max(1);
    let by_memory = (FLUSH_BUILD_MEMORY_BUDGET / per_block).max(1) as usize;
    parallelism.min(chunk_count).min(by_memory).max(1)
}

/// 把本次物化槽位按 [`flush_chunk_rows`] 切分为构建块。
///
/// 每块至多配置的块行数;块数由行数决定,并行度只限制**同时**构建的块数
/// (见 `encode_flush_segments`),不放大单块规模。空槽位(纯 delta 段)同样返回
/// 一个空块:块与段一一对应,供 MANIFEST/安装使用。
fn split_slot_chunks<'a>(
    slots: &'a [usize],
    tuning: &crate::core::options::Tuning,
) -> Vec<&'a [usize]> {
    split_slot_chunks_with(slots, flush_chunk_rows(tuning))
}

/// 按给定块行数切分(便于单测覆盖多块边界;生产经 [`split_slot_chunks`])。
fn split_slot_chunks_with(slots: &[usize], chunk_rows: usize) -> Vec<&[usize]> {
    if slots.is_empty() {
        return vec![&[]];
    }
    // reason: 调用方保证 `chunk_rows > 0`(环境变量非法值已在 `flush_chunk_rows` 回退)。
    slots.chunks(chunk_rows.max(1)).collect()
}

/// 一次增量 flush 提交所需的输入(参数收敛,避免超长参数表)。
struct CommitFlushInput {
    /// 提交前的 MANIFEST。
    previous: Manifest,
    /// 本次物化的未落盘槽位。
    slot_indices: Vec<usize>,
    /// 跨段 delta 条目。
    delta: Vec<crate::persist::msec::DeltaEntry>,
    /// 是否全量重写关系表(首段)。
    full_relations: bool,
    /// 新段创建时刻(Unix 毫秒)。
    now_ms: i64,
}

impl Store {
    /// 增量段 flush:物化未落盘槽位 + delta + 提交 MANIFEST + Checkpoint WAL。
    ///
    /// 旧段保持活跃且 write-once;无新增槽位/ delta / 注册表变化时为空操作。
    /// HNSW 图随段写入 `hidx` 并安装到写状态(`ws.indexes`),未落盘尾部仍由
    /// 调用方暴力扫描。
    ///
    /// # Errors
    /// 只读模式返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(crate) fn flush(&self, ws: &mut WriterState, config: &Config) -> Result<()> {
        if self.read_only {
            return Err(MnemeError::Unsupported {
                feature: "只读模式写入",
            });
        }
        let previous = self.manifest_snapshot();
        let slot_indices = ws.unpersisted_slots();
        let full_relations = previous.segments.is_empty();
        let now_ms = config.clock.now_unix_ms();
        let delta = flush::build_delta(ws, &slot_indices, now_ms, full_relations);
        if slot_indices.is_empty() && delta.is_empty() && !namespace_registry_changed(&previous, ws)
        {
            return Ok(());
        }
        self.commit_flush(
            ws,
            config,
            CommitFlushInput {
                previous,
                slot_indices,
                delta,
                full_relations,
                now_ms,
            },
        )
    }

    /// 编码新段(大 flush 按 [`FLUSH_CHUNK_ROWS`] 切块并行构建)并提交 MANIFEST,
    /// 随后安装索引、清脏并发布新快照。
    fn commit_flush(
        &self,
        ws: &mut WriterState,
        config: &Config,
        input: CommitFlushInput,
    ) -> Result<()> {
        let base_segment_id = input.previous.next_segment_id;
        let chunks = split_slot_chunks(&input.slot_indices, &config.tuning);
        // 有新数据/delta 时写新段;仅注册表变化时只提交 MANIFEST。
        let encoded = self.encode_flush_segments(ws, config, &input, &chunks)?;

        let new_manifest = self.next_manifest(&NextManifestInput {
            previous: &input.previous,
            ws,
            encoded: &encoded,
            chunks: &chunks,
            now_ms: input.now_ms,
        })?;
        manifest_io::commit_manifest(
            self.storage.as_ref(),
            &new_manifest,
            self.hook.as_deref(),
            self.encryption.as_ref(),
        )?;

        // 段文件已原子提交:登记槽位归属与索引,清空 delta 标记;WAL 重置(Checkpoint)。
        self.install_committed(ws, base_segment_id, &chunks, &encoded, &new_manifest);
        Ok(())
    }

    /// 段文件提交后的安装与发布:登记槽位/索引、清脏、发布快照并发 flush 事件。
    fn install_committed(
        &self,
        ws: &mut WriterState,
        base_segment_id: u32,
        chunks: &[&[usize]],
        encoded: &[flush::EncodedSegment],
        new_manifest: &Manifest,
    ) {
        // 本次物化的槽位计入「已落盘」计数(WAL 阈值检查据此判断未落盘行数)。
        ws.note_materialized(chunks.iter().map(|chunk| chunk.len()).sum());
        // 段号从 `base_segment_id` 起连续分配,与 MANIFEST 条目一一对应。
        for (offset, (chunk, segment)) in chunks.iter().zip(encoded).enumerate() {
            let Some(segment_id) = u32::try_from(offset)
                .ok()
                .and_then(|offset| base_segment_id.checked_add(offset))
            else {
                // reason: 段号水位已在 `next_manifest` 经 `next_segment_id` 检查;
                // 此处不可达,保留防御分支避免 release 回绕。
                continue;
            };
            ws.install_segment(
                segment_id,
                chunk,
                segment.index.clone(),
                segment.quant,
                segment.recall_est,
            );
        }
        ws.clear_flush_dirty();
        self.publish(new_manifest);
        // 事件可观测:一次段 flush 提交(新增段数 + 当前 WAL 字节)。
        crate::core::observe::emit(
            self.observer.as_ref(),
            crate::core::observe::Event::Flush {
                segments: encoded.len(),
                wal_bytes: self.wal_bytes(),
            },
        );
    }

    /// 有新增槽位/delta 时编码并写出新段(大 flush 切块并行构建);否则空 `Vec`。
    ///
    /// 跨段 `delta` 与全量关系表只随首段写入(其余段为空),恢复语义不变。
    fn encode_flush_segments(
        &self,
        ws: &WriterState,
        config: &Config,
        input: &CommitFlushInput,
        chunks: &[&[usize]],
    ) -> Result<Vec<flush::EncodedSegment>> {
        if input.slot_indices.is_empty() {
            if input.delta.is_empty() {
                return Ok(Vec::new());
            }
            // 纯 delta 段(无槽位):单段即可,无需并行。
            let encoded = flush::build_segment(
                ws,
                config,
                input.now_ms,
                &SegmentBuildInput {
                    slots: &[],
                    delta: &input.delta,
                    full_relations: input.full_relations,
                    parallelism: 1,
                },
            )?;
            self.write_segment_files(input.previous.next_segment_id, &encoded)?;
            return Ok(vec![encoded]);
        }
        // 单块时用库配置的批内并行;多块时块间已并行,内层传 1 避免嵌套过度订阅
        // (4 块 × 4 线程会争抢 4 核,反而拖慢)。
        let build =
            |chunk: &[usize], first: bool, parallelism: usize| -> Result<flush::EncodedSegment> {
                flush::build_segment(
                    ws,
                    config,
                    input.now_ms,
                    &SegmentBuildInput {
                        slots: chunk,
                        delta: if first { &input.delta } else { &[] },
                        full_relations: first && input.full_relations,
                        parallelism,
                    },
                )
            };
        let encoded: Vec<flush::EncodedSegment> = if chunks.len() <= 1 {
            vec![build(
                chunks.first().copied().unwrap_or(&[]),
                true,
                config.parallelism,
            )?]
        } else {
            // 段间独立,有界并行构建各块(worker 轮转分派),结果按块序回收;
            // 每块在独立线程构建图与编码(设计 04 §8 增量段)。并发块数由内存
            // 预算收紧,避免大维度多块同时驻留触发换页。
            //
            // 默认块级串行(`Tuning.flush_threads == 1`):块级并行与块内 HNSW 批
            // 并行争抢内存带宽,实测嵌套(4 块 × 4 线程)反而更慢;串行块级 + 块内
            // 批并行是 4 核机上的最快组合。块级并行可经 `Tuning.flush_threads`/
            // `MNEME_FLUSH_THREADS` 显式开启(大内存/多核 runner),此时内层传 1
            // 避免过度订阅。
            let threads = flush_build_threads(
                config.dimension.get() as usize,
                flush_chunk_rows(&config.tuning),
                flush_parallelism(&config.tuning),
                chunks.len(),
            );
            let inner = if threads > 1 { 1 } else { config.parallelism };
            std::thread::scope(|scope| {
                let handles: Vec<_> = (0..threads)
                    .map(|worker| {
                        scope.spawn(move || -> Result<Vec<(usize, flush::EncodedSegment)>> {
                            let mut produced = Vec::new();
                            let mut index = worker;
                            while index < chunks.len() {
                                produced.push((index, build(chunks[index], index == 0, inner)?));
                                index += threads;
                            }
                            Ok(produced)
                        })
                    })
                    .collect();
                let mut results: Vec<Option<flush::EncodedSegment>> =
                    (0..chunks.len()).map(|_| None).collect();
                for handle in handles {
                    // reason: 构建线程 panic 时收敛为结构化错误,不把 panic 抛给调用方。
                    let produced = handle.join().map_err(|_| MnemeError::Inconsistent {
                        reason: "flush 段构建线程 panic",
                    })??;
                    for (index, segment) in produced {
                        results[index] = Some(segment);
                    }
                }
                results
                    .into_iter()
                    .map(|segment| {
                        segment.ok_or(MnemeError::Inconsistent {
                            reason: "flush 段构建缺失块",
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })?
        };
        // 段文件串行写出(write-once;段号与 MANIFEST 条目顺序一致)。
        for (offset, segment) in encoded.iter().enumerate() {
            let offset = u32::try_from(offset)
                .map_err(|_| MnemeError::IdExhausted { kind: "segment_id" })?;
            let segment_id = input
                .previous
                .next_segment_id
                .checked_add(offset)
                .ok_or(MnemeError::IdExhausted { kind: "segment_id" })?;
            self.write_segment_files(segment_id, segment)?;
        }
        Ok(encoded)
    }

    /// 写入一个新段的 vsec/msec(以及可选 hidx)文件。
    fn write_segment_files(&self, segment_id: u32, encoded: &flush::EncodedSegment) -> Result<()> {
        let names = [
            format!("{SEGMENTS_DIR}/{}", vsec_name(segment_id)),
            format!("{SEGMENTS_DIR}/{}", msec_name(segment_id)),
            format!("{SEGMENTS_DIR}/{}", hidx_name(segment_id)),
        ];
        self.write_file(&names[0], b"vsec", u64::from(segment_id), &encoded.vsec)?;
        self.write_file(&names[1], b"msec", u64::from(segment_id), &encoded.msec)?;
        if let Some(hidx) = &encoded.hidx {
            self.write_file(&names[2], b"hidx", u64::from(segment_id), hidx)?;
        }
        Ok(())
    }

    /// 备份到目标目录(设计 16 §7)。
    ///
    /// 目标目录必须不存在或为空;先写全部段/MANIFEST/WAL,最后写 `current`,
    /// 中途失败不会留下可打开的备份。备份可被独立 `open`。
    ///
    /// # Errors
    /// 目标非空、目标等于源、或 I/O 失败时返回结构化错误。
    pub(crate) fn backup_to(&self, target: &Path) -> Result<crate::memory::ops::BackupReport> {
        if target == self.root {
            return Err(MnemeError::Config {
                reason: "备份目录不能是库目录本身",
            });
        }
        if storage::exists(target)? && !storage::list_dir(target, "")?.is_empty() {
            return Err(MnemeError::Busy("备份目标目录非空"));
        }
        storage::ensure_dir(target)?;
        storage::ensure_dir(&target.join(SEGMENTS_DIR))?;
        storage::ensure_dir(&target.join(WAL_DIR))?;

        let (version, manifest) = self.versioned_manifest();

        let mut sink = CopySink {
            root: &self.root,
            target,
            counts: CopyCounts::default(),
            hardlinked: !manifest.segments.is_empty(),
        };
        copy_segment_files(&mut sink, &manifest)?;
        sink.copy_required(&manifest_name(version))?;
        // WAL 文件集可缺省(只读实例/刚 Checkpoint 后);逐文件复制。
        for rel in super::wal_writer::wal_files(self.storage.as_ref())? {
            copy_optional(&self.root, target, &rel, &mut sink.counts)?;
        }
        let CopyCounts { files, bytes } = sink.counts;
        // `current` 最后写:中途失败则备份不可打开,不会误认为完整。
        let current = version.to_string();
        storage::write_atomic(target, CURRENT_FILE, current.as_bytes())?;

        Ok(crate::memory::ops::BackupReport {
            files: files + 1,
            bytes: bytes + current.len() as u64,
            hardlinked: sink.hardlinked,
        })
    }

    /// 由当前写状态、刚物化的段(可缺省)与新增槽位构造下一个 MANIFEST 版本。
    ///
    /// # Errors
    /// 段号或 MANIFEST 版本水位耗尽时返回 [`MnemeError::IdExhausted`]
    /// (FC-PERSIST-ERR-012)。
    fn next_manifest(&self, input: &NextManifestInput<'_>) -> Result<Manifest> {
        let mut segments = input.previous.segments.clone();
        let mut next_segment_id = input.previous.next_segment_id;
        for (chunk, encoded) in input.chunks.iter().zip(input.encoded) {
            segments.push(segment_entry(
                &SegmentEntryInput {
                    segment_id: next_segment_id,
                    ws: input.ws,
                    slot_indices: chunk,
                    now_ms: input.now_ms,
                },
                encoded,
            ));
            next_segment_id = crate::persist::manifest::next_segment_id(next_segment_id)?;
        }
        Ok(Manifest {
            dimension: self.dimension,
            metric: self.metric,
            stopwords: input.previous.stopwords,
            next_rel_kind: input.ws.next_rel_kind,
            manifest_version: crate::persist::manifest::next_manifest_version(
                input.previous.manifest_version,
            )?,
            watermark_seqno: input.ws.seqno.get(),
            next_rowid: input.ws.next_rowid,
            next_segment_id,
            next_ns_id: input.ws.next_ns_id,
            namespaces: manifest_namespaces(input.ws),
            rel_kinds: manifest_rel_kinds(input.ws),
            segments,
        })
    }

    /// 发布新 MANIFEST 快照并重置 WAL(Checkpoint)。
    fn publish(&self, new_manifest: &Manifest) {
        self.publish_manifest(new_manifest);
        let mut wal = self
            .wal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // reason: Checkpoint 为空间回收;旧帧均 ≤ watermark,恢复时跳过。段与
        // MANIFEST 已提交,重置失败不阻断 flush(句柄安全由 `WalWriter::reset`
        // 内部重建/停用保证,FC-PERSIST-INV-005)。
        let _ = wal.reset().ok();
    }

    /// 只发布 MANIFEST 快照(不重置 WAL;compaction 用,未落盘尾部仍在 WAL 中)。
    pub(super) fn publish_manifest(&self, new_manifest: &Manifest) {
        let mut guard = self
            .manifest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.version = new_manifest.manifest_version;
        guard.manifest = new_manifest.clone();
    }
}

/// [`Store::next_manifest`] 的输入(参数收敛)。
struct NextManifestInput<'a> {
    /// 提交前的 MANIFEST。
    previous: &'a Manifest,
    /// 写状态(水位/注册表)。
    ws: &'a WriterState,
    /// 本批物化的新段(仅注册表变化时为空;与 `chunks` 一一对应)。
    encoded: &'a [flush::EncodedSegment],
    /// 各新段包含的槽位(与 `encoded` 一一对应)。
    chunks: &'a [&'a [usize]],
    /// 新段创建时刻(Unix 毫秒)。
    now_ms: i64,
}

/// 单段 MANIFEST 条目的输入(参数收敛)。
struct SegmentEntryInput<'a> {
    /// 本段编号。
    segment_id: u32,
    /// 写状态(水位)。
    ws: &'a WriterState,
    /// 本段包含的全局槽位。
    slot_indices: &'a [usize],
    /// 段创建时刻(Unix 毫秒)。
    now_ms: i64,
}

/// 新段的 MANIFEST 条目。
fn segment_entry(input: &SegmentEntryInput<'_>, encoded: &flush::EncodedSegment) -> SegmentEntry {
    let (min_seqno, max_seqno) = seqno_range(input.ws, input.slot_indices);
    SegmentEntry {
        segment_id: input.segment_id,
        format_version: FORMAT_VERSION,
        row_count: input.slot_indices.len() as u64,
        min_seqno,
        max_seqno,
        created_ms: input.now_ms,
        vsec_crc: crc32(&encoded.vsec),
        msec_crc: crc32(&encoded.msec),
        hidx_crc: encoded.hidx.as_deref().map_or(0, crc32),
        entry_slot: encoded.entry_slot,
        entry_level: encoded.entry_level,
    }
}

/// 注册表条目(按 `NsId` 排序)。
fn manifest_namespaces(ws: &WriterState) -> Vec<NsEntry> {
    let mut namespaces: Vec<NsEntry> = ws
        .ns_registry
        .iter()
        .map(|(id, path)| NsEntry {
            ns_id: id.get(),
            path: Arc::clone(path),
        })
        .collect();
    namespaces.sort_by_key(|entry| entry.ns_id);
    namespaces
}

/// 由写状态的关系类型注册表构造 MANIFEST 条目(按编号升序,确定性编码)。
fn manifest_rel_kinds(ws: &WriterState) -> Vec<RelKindEntry> {
    let mut entries: Vec<RelKindEntry> = ws
        .rel_kind_names
        .iter()
        .map(|(&kind, name)| RelKindEntry {
            kind,
            name: Arc::clone(name),
        })
        .collect();
    entries.sort_by_key(|entry| entry.kind);
    entries
}

/// 备份已复制文件数与字节数。
#[derive(Default)]
struct CopyCounts {
    files: usize,
    bytes: u64,
}

/// 备份 MANIFEST 所列段(vsec/msec/可选 hidx);必存文件缺失即失败。
fn copy_segment_files(sink: &mut CopySink<'_>, manifest: &Manifest) -> Result<()> {
    for segment in &manifest.segments {
        // 被 MANIFEST 引用的段必须存在;缺失即备份不可信,绝不静默产出残档。
        sink.link_or_copy_required(&format!("{SEGMENTS_DIR}/{}", vsec_name(segment.segment_id)))?;
        sink.link_or_copy_required(&format!("{SEGMENTS_DIR}/{}", msec_name(segment.segment_id)))?;
        if segment.hidx_crc != 0 {
            sink.link_or_copy_required(&format!(
                "{SEGMENTS_DIR}/{}",
                hidx_name(segment.segment_id)
            ))?;
        }
    }
    Ok(())
}

/// 复制一个可选文件(如只读实例中不存在的 WAL);缺失则跳过。
fn copy_optional(root: &Path, target: &Path, rel: &str, counts: &mut CopyCounts) -> Result<()> {
    if let Some(content) = storage::read_file_opt(root, rel)? {
        counts.files += 1;
        counts.bytes += content.len() as u64;
        storage::write_atomic(target, rel, &content)?;
    }
    Ok(())
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

/// 命名空间注册表相对 MANIFEST 是否有变化(注销/新增需要独立提交,即使无新槽位)。
fn namespace_registry_changed(previous: &Manifest, ws: &WriterState) -> bool {
    if previous.namespaces.len() != ws.ns_registry.len() {
        return true;
    }
    previous.namespaces.iter().any(|entry| {
        ws.ns_registry
            .get(&crate::core::types::NsId::new(entry.ns_id))
            .is_none_or(|path| path.as_ref() != entry.path.as_ref())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 切块覆盖全部槽位且保序、不重叠;空槽位(纯 delta 段)仍产生一个空块;
    /// 块行数尊重 `Tuning.flush_chunk_rows` 配置。
    #[test]
    fn split_slot_chunks_covers_all_slots_in_order() {
        let tuning = crate::core::options::Tuning::default();
        let slots: Vec<usize> = (0..10_000).collect();
        let chunks = split_slot_chunks(&slots, &tuning);
        assert!(!chunks.is_empty());
        let flat: Vec<usize> = chunks
            .iter()
            .flat_map(|chunk| chunk.iter().copied())
            .collect();
        assert_eq!(flat, slots, "切块必须按序覆盖全部槽位且不重叠/不遗漏");

        let empty = split_slot_chunks(&[], &tuning);
        assert_eq!(empty.len(), 1, "空槽位需要一个空块与纯 delta 段一一对应");
        assert!(empty[0].is_empty());

        let small_chunks = {
            let custom = crate::core::options::Tuning {
                flush_chunk_rows: 1_024,
                ..tuning
            };
            split_slot_chunks(&slots, &custom)
        };
        assert_eq!(small_chunks.len(), 10, "块行数配置应生效");
    }

    /// 并行块数受内存预算约束:1536 维 65,536 行块单块约 0.82GB,6GB 预算下
    /// 至多 7 块并行;小维度(预算充足)不被额外收紧。
    #[test]
    fn flush_build_threads_bounded_by_memory_budget() {
        // 1536 维 × 65,536 行:约 0.82GB/块 → 6GB / 0.82GB = 7。
        assert_eq!(flush_build_threads(1536, 65_536, 8, 16), 7);
        // 16 核也不超过预算允许的并发。
        assert_eq!(flush_build_threads(1536, 65_536, 16, 16), 7);
        // 小维度:预算充足时只受并行度与块数约束。
        assert_eq!(flush_build_threads(128, 65_536, 8, 16), 8);
        assert_eq!(flush_build_threads(128, 65_536, 8, 3), 3);
        // 退化输入:至少 1 块。
        assert_eq!(flush_build_threads(0, 0, 0, 0), 1);
    }

    /// FC-PERSIST-STA-004:块数由行数决定、**每块不超过 `chunk_rows`**;并行度只限
    /// 同时构建的块数,不得放大单块规模(修复「并行度越大块越大」的行为)。
    #[test]
    fn split_slot_chunks_limits_block_rows() {
        let slots: Vec<usize> = (0..10_000).collect();
        let chunks = split_slot_chunks_with(&slots, 1_024);
        assert_eq!(chunks.len(), 10, "10_000 行按 1_024 行应切 10 块");
        assert!(
            chunks.iter().all(|chunk| chunk.len() <= 1_024),
            "每块不得超过块行数上限"
        );
        assert_eq!(
            chunks.iter().map(|chunk| chunk.len()).sum::<usize>(),
            10_000
        );
    }

    /// FC-LIFE-POST-008:硬链接失败(目标已存在/跨盘)时回退逐文件复制,
    /// `hardlinked` 如实置 `false`,产物内容正确。
    #[test]
    fn hardlink_failure_falls_back_to_copy() {
        let root = tempfile::tempdir().expect("root");
        let target = tempfile::tempdir().expect("target");
        let rel = format!("{SEGMENTS_DIR}/seg_000001.vsec");
        let source = storage::resolve(root.path(), &rel).expect("source path");
        storage::ensure_dir(source.parent().expect("parent")).expect("segments dir");
        let target_dir = storage::resolve(target.path(), &rel).expect("target path");
        storage::ensure_dir(target_dir.parent().expect("parent")).expect("target dir");
        let content = b"fake segment bytes";
        std::fs::write(&source, content).expect("write source");
        // 目标已存在同名文件 → `hard_link` 失败 → 走复制回退。
        std::fs::write(&target_dir, b"stale").expect("write stale");

        let mut sink = CopySink {
            root: root.path(),
            target: target.path(),
            counts: CopyCounts::default(),
            hardlinked: true,
        };
        sink.link_or_copy_required(&rel).expect("copy fallback");
        assert!(!sink.hardlinked, "硬链接失败必须如实报告回退");
        assert_eq!(sink.counts.files, 1);
        assert_eq!(sink.counts.bytes, content.len() as u64);
        assert_eq!(std::fs::read(&target_dir).expect("read"), content);
    }
}
