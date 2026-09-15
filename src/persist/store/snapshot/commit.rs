//! 增量段提交:物化槽位切块编码、新段写出、MANIFEST 提交同安装发布。

use crate::core::error::{MnemeError, Result};
use crate::memory::config::Config;
use crate::memory::table::{InstallSegmentInput, WriterState};
use crate::persist::flush::{self, SegmentBuildInput};
use crate::persist::manifest::Manifest;
use crate::persist::storage::{SEGMENTS_DIR, hidx_name, msec_name, vsec_name};

use super::super::Store;
use super::super::manifest_io;
use super::chunk::{flush_build_threads, flush_chunk_rows, flush_parallelism, split_slot_chunks};
use super::manifest::{NextManifestInput, namespace_registry_changed};

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

/// [`Store::install_committed`] 的输入参数。
struct InstallCommittedInput<'a> {
    /// 写状态(安装段索引与清脏)。
    ws: &'a mut WriterState,
    /// 首个新段的段号(MANIFEST 中已提交)。
    base_segment_id: u32,
    /// 本次物化的槽位分块(与 `encoded` 一一对应)。
    chunks: &'a [&'a [usize]],
    /// 各块段编码产物。
    encoded: &'a [flush::EncodedSegment],
    /// 已提交的新 MANIFEST。
    new_manifest: &'a Manifest,
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
        self.install_committed(InstallCommittedInput {
            ws,
            base_segment_id,
            chunks: &chunks,
            encoded: &encoded,
            new_manifest: &new_manifest,
        });
        Ok(())
    }

    /// 段文件提交后的安装与发布:登记槽位/索引、清脏、发布快照并发 flush 事件。
    fn install_committed(&self, input: InstallCommittedInput<'_>) {
        let InstallCommittedInput {
            ws,
            base_segment_id,
            chunks,
            encoded,
            new_manifest,
        } = input;
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
            ws.install_segment(InstallSegmentInput {
                segment_id,
                slot_indices: chunk,
                index: segment.index.clone(),
                quant: segment.quant,
                recall_est: segment.recall_est,
            });
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
            return self.encode_delta_only_segment(ws, config, input);
        }
        let encoded = encode_slot_chunks(ws, config, input, chunks)?;
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

    /// 纯 delta 段(无槽位):单段即可,无需并行。
    fn encode_delta_only_segment(
        &self,
        ws: &WriterState,
        config: &Config,
        input: &CommitFlushInput,
    ) -> Result<Vec<flush::EncodedSegment>> {
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
        Ok(vec![encoded])
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
}

/// 按块构建段的上下文(参数收敛):写状态、配置与本次 flush 的 delta/关系标志。
#[derive(Clone, Copy)]
struct ChunkBuildContext<'a> {
    /// 写状态。
    ws: &'a WriterState,
    /// 库配置。
    config: &'a Config,
    /// 新段创建时刻(Unix 毫秒)。
    now_ms: i64,
    /// 跨段 delta 条目。
    delta: &'a [crate::persist::msec::DeltaEntry],
    /// 是否全量重写关系表(首段)。
    full_relations: bool,
}

/// 构建单个段块:首块携带跨段 `delta` 与全量关系表。
fn build_chunk(
    context: &ChunkBuildContext<'_>,
    chunk: &[usize],
    first: bool,
    parallelism: usize,
) -> Result<flush::EncodedSegment> {
    flush::build_segment(
        context.ws,
        context.config,
        context.now_ms,
        &SegmentBuildInput {
            slots: chunk,
            delta: if first { context.delta } else { &[] },
            full_relations: first && context.full_relations,
            parallelism,
        },
    )
}

/// 有槽位段:单块走块内批并行;多块按 `Tuning` 有界并行构建。
fn encode_slot_chunks(
    ws: &WriterState,
    config: &Config,
    input: &CommitFlushInput,
    chunks: &[&[usize]],
) -> Result<Vec<flush::EncodedSegment>> {
    let context = ChunkBuildContext {
        ws,
        config,
        now_ms: input.now_ms,
        delta: &input.delta,
        full_relations: input.full_relations,
    };
    // 单块时用库配置的批内并行;多块时块间已并行,内层传 1 避免嵌套过度订阅
    // (4 块 × 4 线程会争抢 4 核,反而拖慢)。
    if chunks.len() <= 1 {
        return Ok(vec![build_chunk(
            &context,
            chunks.first().copied().unwrap_or(&[]),
            true,
            config.parallelism,
        )?]);
    }
    build_chunks_parallel(&context, chunks)
}

/// 段间独立,有界并行构建各块(worker 轮转分派),结果按块序回收。
///
/// 每块在独立线程构建图与编码(设计 04 §8 增量段);并发块数由内存预算收紧,
/// 避免大维度多块同时驻留触发换页。默认块级串行(`Tuning.flush_threads == 1`):
/// 块级并行与块内 HNSW 批并行争抢内存带宽,实测嵌套(4 块 × 4 线程)反而更慢;
/// 串行块级 + 块内批并行是 4 核机上的最快组合。块级并行可经 `Tuning.flush_threads`
/// 显式开启(大内存/多核 runner;重载调参一律走配置,库不读环境变量),
/// 此时内层传 1 避免过度订阅。
fn build_chunks_parallel(
    context: &ChunkBuildContext<'_>,
    chunks: &[&[usize]],
) -> Result<Vec<flush::EncodedSegment>> {
    let threads = flush_build_threads(
        context.config.dimension.get() as usize,
        flush_chunk_rows(&context.config.tuning),
        flush_parallelism(&context.config.tuning),
        chunks.len(),
    );
    let inner = if threads > 1 {
        1
    } else {
        context.config.parallelism
    };
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|worker| {
                scope.spawn(move || -> Result<Vec<(usize, flush::EncodedSegment)>> {
                    let mut produced = Vec::new();
                    let mut index = worker;
                    while index < chunks.len() {
                        produced.push((
                            index,
                            build_chunk(context, chunks[index], index == 0, inner)?,
                        ));
                        index += threads;
                    }
                    Ok(produced)
                })
            })
            .collect();
        collect_chunk_results(handles, chunks.len())
    })
}

/// 段块构建 worker 的句柄:产物为 `(块下标, 段)` 列表。
type ChunkBuildHandle<'scope> =
    std::thread::ScopedJoinHandle<'scope, Result<Vec<(usize, flush::EncodedSegment)>>>;

/// 回收各 worker 产物:按块序归位,缺失块收敛为结构化错误。
fn collect_chunk_results(
    handles: Vec<ChunkBuildHandle<'_>>,
    chunk_count: usize,
) -> Result<Vec<flush::EncodedSegment>> {
    let mut results: Vec<Option<flush::EncodedSegment>> = (0..chunk_count).map(|_| None).collect();
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
        .collect()
}
