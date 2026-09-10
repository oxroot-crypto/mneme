//! L2 持久化协调句柄 `Store`(`store.rs`)。
//!
//! `Store` 持有目录、独占锁、WAL 写入器与当前 MANIFEST,实现内存引擎的
//! [`PersistHook`](crate::memory::table::PersistHook):
//!
//! - 每条写操作由 `Table::write_tx` 交给 [`Store::log`],先追加 WAL 再允许可见
//!   (WAL-before-visible,设计 04 §3.1);多操作批以 `BatchBegin/Commit` 包裹;
//! - [`Store::flush`] 把整个写状态物化为新段并提交新 MANIFEST(全量快照,L2 兜底),
//!   随后重置 WAL(Checkpoint)。
//!
//! > 该模块是对设计 04 §1 模块清单的必要补充(L2 需要一个协调句柄);已同步到
//! > 01 §4 与 04 §1。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::FsyncPolicy;
use crate::memory::config::Config;
use crate::memory::table::{PersistHook, SlotData, WriteOp, WriterState};
use crate::persist::flush;
use crate::persist::hook::{FsyncHook, IoAction};
use crate::persist::manifest::{self, Manifest, NsEntry, SegmentEntry};
use crate::persist::msec::EntryData;
use crate::persist::recover::{self, SegmentBytes};
use crate::persist::storage::{
    self, CURRENT_FILE, FileLock, MANIFEST_KEEP, SEGMENTS_DIR, WAL_DIR, manifest_name, msec_name,
    parse_manifest_name, vsec_name,
};
use crate::persist::trash;
use crate::persist::wal::{self, FrameKind};
use crate::persist::{FORMAT_VERSION, crc32};

/// 单个 WAL 文件名(全量快照模式下只需一个,Checkpoint 时重建)。
const WAL_FILE: &str = "wal/wal_000001.log";

/// WAL 写入器:持有当前 WAL 文件句柄,按 [`FsyncPolicy`] 决定落盘时机。
struct WalWriter {
    file: std::fs::File,
    policy: FsyncPolicy,
    dimension: u32,
    metric: Metric,
    hook: Option<Arc<dyn FsyncHook>>,
}

impl WalWriter {
    /// 打开既有 WAL(追加)或新建(写文件头)。
    ///
    /// 既有 WAL 的头部维度/度量与当前库不符(或头部损坏)时重建;否则保留既有帧,
    /// 由 `Store::open` 回放后再继续追加——绝不在此截断已 fsync 的帧。
    ///
    /// # Errors
    /// I/O 失败返回 [`MnemeError::Io`]。
    fn open_or_create(
        root: &Path,
        dimension: u32,
        metric: Metric,
        policy: FsyncPolicy,
        hook: Option<Arc<dyn FsyncHook>>,
    ) -> Result<Self> {
        storage::ensure_dir(&root.join(WAL_DIR))?;
        let path = storage::resolve(root, WAL_FILE)?;
        let reuse = storage::read_file_opt(root, WAL_FILE)?
            .filter(|bytes| bytes.len() >= wal::FILE_HEADER_LEN)
            .and_then(|bytes| wal::parse_file_header(&bytes).ok())
            .is_some_and(|header| header.dimension == dimension && header.metric == metric);
        if reuse {
            // 以 `write`(而非 `append`)打开:Windows 下 append-only 句柄缺少
            // FILE_WRITE_DATA,`set_len`(Checkpoint 重置)会被拒绝。
            let mut file = std::fs::OpenOptions::new().write(true).open(&path)?;
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::End(0))?;
            return Ok(Self {
                file,
                policy,
                dimension,
                metric,
                hook,
            });
        }
        let header = wal::encode_file_header(dimension, metric);
        let mut file = std::fs::File::create(&path)?;
        use std::io::Write as _;
        if let Some(hook) = &hook {
            hook.before(IoAction::Write {
                file: WAL_FILE,
                offset: 0,
                len: header.len(),
            })?;
        }
        file.write_all(&header)?;
        file.sync_all()?;
        Ok(Self {
            file,
            policy,
            dimension,
            metric,
            hook,
        })
    }

    /// 追加一帧,并按策略 fsync。返回写入后的文件长度。
    ///
    /// # Errors
    /// I/O 失败返回 [`MnemeError::Io`]。
    fn append(&mut self, seqno: u64, kind: FrameKind, payload: &[u8]) -> Result<u64> {
        use std::io::Write as _;
        let frame = wal::encode_frame(seqno, kind, payload);
        let offset = self.file.metadata()?.len();
        if let Some(hook) = &self.hook {
            hook.before(IoAction::Write {
                file: WAL_FILE,
                offset,
                len: frame.len(),
            })?;
        }
        self.file.write_all(&frame)?;
        // `Always`/`Batched` 均在此同步:同步次数不弱于设计承诺(更强持久性无害)。
        if matches!(self.policy, FsyncPolicy::Always | FsyncPolicy::Batched(_)) {
            if let Some(hook) = &self.hook {
                hook.before(IoAction::Fsync { file: WAL_FILE })?;
            }
            self.file.sync_all()?;
        }
        Ok(self.file.metadata()?.len())
    }

    /// 重置 WAL(Checkpoint):截断为空并重写文件头。
    ///
    /// # Errors
    /// I/O 失败返回 [`MnemeError::Io`]。
    fn reset(&mut self) -> Result<()> {
        self.file.set_len(0)?;
        use std::io::{Seek, SeekFrom, Write as _};
        self.file.seek(SeekFrom::Start(0))?;
        let header = wal::encode_file_header(self.dimension, self.metric);
        self.file.write_all(&header)?;
        self.file.sync_all()?;
        Ok(())
    }
}

/// 当前 MANIFEST 快照(受 `Mutex` 保护)。
struct ManifestState {
    version: u64,
    manifest: Manifest,
}

/// L2 持久化协调句柄。
pub(crate) struct Store {
    root: PathBuf,
    lock: Mutex<Option<FileLock>>,
    wal: Mutex<WalWriter>,
    manifest: Mutex<ManifestState>,
    dimension: u32,
    metric: Metric,
    read_only: bool,
    hook: Option<Arc<dyn FsyncHook>>,
}

impl Store {
    /// 打开或新建持久库。
    ///
    /// 返回 `(store, 初始写状态, 维度, 度量)`。已存在库以 MANIFEST 的维度/度量为准,
    /// 与调用方请求不符时拒绝打开(设计 16 §3)。
    ///
    /// # Errors
    /// 目录不可用、锁被占、MANIFEST/段损坏(且 fail-fast)或维度冲突时返回结构化错误。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open(
        root: &Path,
        requested_dimension: Option<u32>,
        requested_metric: Option<Metric>,
        fsync: FsyncPolicy,
        read_only: bool,
        verify_on_open: bool,
        fail_fast_on_corruption: bool,
        hook: Option<Arc<dyn FsyncHook>>,
    ) -> Result<(Arc<Store>, WriterState, u32, Metric)> {
        storage::ensure_dir(root)?;
        storage::ensure_dir(&root.join(SEGMENTS_DIR))?;
        storage::ensure_dir(&root.join(WAL_DIR))?;
        storage::ensure_dir(&root.join(storage::TRASH_DIR))?;

        let lock = if read_only {
            None
        } else {
            Some(FileLock::acquire(root)?)
        };
        trash::purge(root)?;

        let loaded = load_manifest(root)?;
        let (manifest, version) = match loaded {
            Some((manifest, version)) => {
                if let Some(dimension) = requested_dimension
                    && dimension != manifest.dimension
                {
                    return Err(MnemeError::DimensionMismatch {
                        expected: manifest.dimension,
                        got: dimension as usize,
                    });
                }
                if let Some(metric) = requested_metric
                    && metric != manifest.metric
                {
                    return Err(MnemeError::MetricMismatch {
                        existing: manifest.metric,
                        requested: metric,
                    });
                }
                (manifest, version)
            }
            None => {
                // 无 MANIFEST 但可能有 WAL(崩溃在首次 flush 前):以 WAL 头为准。
                let wal_header = storage::read_file_opt(root, WAL_FILE)?
                    .and_then(|bytes| wal::parse_file_header(&bytes).ok());
                let dimension = match (requested_dimension, wal_header) {
                    (_, None) => requested_dimension.ok_or(MnemeError::Config {
                        reason: "新建持久库必须指定维度",
                    })?,
                    (Some(d), Some(header)) if header.dimension != d => {
                        return Err(MnemeError::DimensionMismatch {
                            expected: header.dimension,
                            got: d as usize,
                        });
                    }
                    (Some(d), Some(_)) => d,
                    (None, Some(header)) => header.dimension,
                };
                let metric = match (requested_metric, wal_header) {
                    (_, None) => requested_metric.unwrap_or(Metric::Cosine),
                    (Some(m), Some(header)) if header.metric != m => {
                        return Err(MnemeError::MetricMismatch {
                            existing: header.metric,
                            requested: m,
                        });
                    }
                    (Some(m), Some(_)) => m,
                    (None, Some(header)) => header.metric,
                };
                let manifest = Manifest {
                    dimension,
                    metric,
                    next_rel_kind: 16,
                    manifest_version: 0,
                    watermark_seqno: 0,
                    next_rowid: 0,
                    next_segment_id: 0,
                    next_ns_id: 1,
                    namespaces: Vec::new(),
                    rel_kinds: Vec::new(),
                    segments: Vec::new(),
                };
                (manifest, 0)
            }
        };

        // 载入段并重建写状态。
        let mut state = recover::empty_state(&manifest);
        let segments = read_segment_bytes(root, &manifest)?;
        recover::load_segments(
            &mut state,
            &segments,
            verify_on_open,
            fail_fast_on_corruption,
        )?;

        // 回放 WAL(仅 seqno > watermark)。
        if let Some(bytes) = storage::read_file_opt(root, WAL_FILE)? {
            recover::replay_wal(&mut state, &bytes, manifest.watermark_seqno)?;
        }

        let wal = WalWriter::open_or_create(
            root,
            manifest.dimension,
            manifest.metric,
            fsync,
            hook.clone(),
        )?;
        let store = Arc::new(Store {
            root: root.to_path_buf(),
            lock: Mutex::new(lock),
            wal: Mutex::new(wal),
            manifest: Mutex::new(ManifestState {
                version,
                manifest: manifest.clone(),
            }),
            dimension: manifest.dimension,
            metric: manifest.metric,
            read_only,
            hook,
        });
        Ok((store, state, manifest.dimension, manifest.metric))
    }

    /// 库根目录。
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// 释放独占锁(`close` 调用;幂等)。
    pub(crate) fn release_lock(&self) {
        let mut guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = None;
    }

    /// 原子写一个库内文件,前置触发 `FsyncHook`(测试崩溃注入)。
    fn write_file(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        if let Some(hook) = &self.hook {
            hook.before(IoAction::Write {
                file: rel,
                offset: 0,
                len: bytes.len(),
            })?;
        }
        storage::write_atomic(&self.root, rel, bytes)
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

        let (version, manifest) = {
            let guard = self
                .manifest
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (guard.version, guard.manifest.clone())
        };

        let mut files = 0_usize;
        let mut bytes = 0_u64;
        let mut copy = |rel: &str| -> Result<()> {
            if let Some(content) = storage::read_file_opt(&self.root, rel)? {
                bytes += content.len() as u64;
                files += 1;
                storage::write_atomic(target, rel, &content)?;
            }
            Ok(())
        };

        for segment in &manifest.segments {
            copy(&format!("{SEGMENTS_DIR}/{}", vsec_name(segment.segment_id)))?;
            copy(&format!("{SEGMENTS_DIR}/{}", msec_name(segment.segment_id)))?;
        }
        copy(&manifest_name(version))?;
        copy(WAL_FILE)?;
        // `current` 最后写:中途失败则备份不可打开,不会误认为完整。
        storage::write_atomic(target, CURRENT_FILE, version.to_string().as_bytes())?;
        files += 1;
        bytes += version.to_string().len() as u64;

        Ok(crate::memory::ops::BackupReport {
            files,
            bytes,
            hardlinked: false,
        })
    }

    /// 全量快照 flush:写新段 + 提交 MANIFEST + 重置 WAL(设计 04 §3.2)。
    ///
    /// # Errors
    /// 只读模式返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(crate) fn flush(&self, ws: &WriterState, config: &Config) -> Result<()> {
        if self.read_only {
            return Err(MnemeError::Unsupported {
                feature: "只读模式写入",
            });
        }
        let created_unix_ms = config.clock.now_unix_ms();
        let (vsec_bytes, msec_bytes) = flush::build_segment(ws, config, created_unix_ms)?;

        let manifest_state = {
            let guard = self
                .manifest
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.manifest.clone()
        };
        let segment_id = manifest_state.next_segment_id;

        self.write_file(
            &format!("{SEGMENTS_DIR}/{}", vsec_name(segment_id)),
            &vsec_bytes,
        )?;
        self.write_file(
            &format!("{SEGMENTS_DIR}/{}", msec_name(segment_id)),
            &msec_bytes,
        )?;

        let (min_seqno, max_seqno) = seqno_range(ws);
        let mut namespaces: Vec<NsEntry> = ws
            .ns_registry
            .iter()
            .map(|(id, path)| NsEntry {
                ns_id: id.get(),
                path: Arc::clone(path),
            })
            .collect();
        namespaces.sort_by_key(|entry| entry.ns_id);

        let new_manifest = Manifest {
            dimension: self.dimension,
            metric: self.metric,
            next_rel_kind: manifest_state.next_rel_kind,
            manifest_version: manifest_state.manifest_version + 1,
            watermark_seqno: ws.seqno.get(),
            next_rowid: ws.next_rowid,
            next_segment_id: segment_id + 1,
            next_ns_id: ws.next_ns_id,
            namespaces,
            rel_kinds: manifest_state.rel_kinds.clone(),
            segments: vec![SegmentEntry {
                segment_id,
                format_version: FORMAT_VERSION,
                row_count: ws.slots.len() as u64,
                min_seqno,
                max_seqno,
                created_ms: created_unix_ms,
                vsec_crc: crc32(&vsec_bytes),
                msec_crc: crc32(&msec_bytes),
                hidx_crc: 0,
                entry_slot: 0,
                entry_level: 0,
            }],
        };
        commit_manifest(&self.root, &new_manifest, self.hook.as_deref())?;

        // 旧段进入 trash 并清理;WAL 重置(所有覆盖条目已随快照物化)。
        let old_segments: Vec<String> = manifest_state
            .segments
            .iter()
            .flat_map(|segment| [vsec_name(segment.segment_id), msec_name(segment.segment_id)])
            .collect();
        trash::move_to_trash(&self.root, &old_segments)?;
        trash::purge(&self.root)?;

        {
            let mut guard = self
                .manifest
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.version = new_manifest.manifest_version;
            guard.manifest = new_manifest;
        }
        let mut wal = self
            .wal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        wal.reset()?;
        Ok(())
    }
}

impl PersistHook for Store {
    fn log(&self, ops: &[WriteOp]) -> Result<()> {
        if self.read_only {
            return Err(MnemeError::Unsupported {
                feature: "只读模式写入",
            });
        }
        if ops.is_empty() {
            return Ok(());
        }
        let mut wal = self
            .wal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let batch = ops.len() > 1;
        if batch {
            wal.append(
                0,
                FrameKind::BatchBegin,
                &wal::encode_batch_begin(ops.len() as u32),
            )?;
        }
        for op in ops {
            match op {
                WriteOp::NsRegister { ns_id, path } => {
                    wal.append(
                        0,
                        FrameKind::NsRegister,
                        &wal::encode_ns_register(*ns_id, path),
                    )?;
                }
                WriteOp::Insert { slot } => {
                    let entry = entry_from_slot(slot);
                    let payload = wal::encode_insert(&entry, &slot.vector)?;
                    wal.append(slot.seqno.get(), FrameKind::Insert, &payload)?;
                }
                WriteOp::DeleteRow { rowid, seqno, .. } => {
                    wal.append(
                        seqno.get(),
                        FrameKind::DeleteRow,
                        &wal::encode_delete_row(rowid.get()),
                    )?;
                }
            }
        }
        if batch {
            let crc = crc32(&[ops.len() as u8]);
            wal.append(
                0,
                FrameKind::BatchCommit,
                &wal::encode_batch_commit(ops.len() as u32, crc),
            )?;
        }
        Ok(())
    }
}

/// 由槽位构造 WAL/msec 记录体。
fn entry_from_slot(slot: &SlotData) -> EntryData {
    EntryData {
        rowid: slot.rowid,
        seqno: slot.seqno,
        ns_id: slot.ns_id,
        key: slot.key.clone(),
        text: slot.text.clone(),
        meta: slot.meta.clone(),
        created_at_ms: slot.created_at,
        expires_at_ms: slot.expires_at,
        importance: Some(slot.importance),
        access: None,
        valid_time: Some((slot.valid_from, slot.valid_to)),
        confidence: Some(slot.confidence),
        provenance: slot.provenance.clone(),
    }
}

/// 计算槽位的 `(min_seqno, max_seqno)`。
fn seqno_range(ws: &WriterState) -> (u64, u64) {
    let mut min = u64::MAX;
    let mut max = 0_u64;
    for slot in ws.slots.iter() {
        let value = slot.seqno.get();
        min = min.min(value);
        max = max.max(value);
    }
    if ws.slots.is_empty() {
        (0, 0)
    } else {
        (min, max)
    }
}

/// 读取 `current` 或扫描目录得到最新合法 MANIFEST;不存在返回 `None`。
fn load_manifest(root: &Path) -> Result<Option<(Manifest, u64)>> {
    if let Some(bytes) = storage::read_file_opt(root, CURRENT_FILE)? {
        let text = String::from_utf8_lossy(&bytes);
        if let Ok(version) = text.trim().parse::<u64>()
            && let Some(bytes) = storage::read_file_opt(root, &manifest_name(version))?
            && let Ok(manifest) = manifest::parse(&bytes)
        {
            return Ok(Some((manifest, version)));
        }
    }
    // 回退:扫描目录取最大的、CRC 合法的版本。
    let mut best: Option<(Manifest, u64)> = None;
    for name in storage::list_dir(root, "")? {
        let Some(version) = parse_manifest_name(&name) else {
            continue;
        };
        if let Some(bytes) = storage::read_file_opt(root, &name)?
            && let Ok(manifest) = manifest::parse(&bytes)
            && best
                .as_ref()
                .is_none_or(|(_, best_version)| version > *best_version)
        {
            best = Some((manifest, version));
        }
    }
    Ok(best)
}

/// 提交 MANIFEST:写 `MANIFEST.<v>` → 写 `current` → 保留最近 `MANIFEST_KEEP` 版。
fn commit_manifest(root: &Path, manifest: &Manifest, hook: Option<&dyn FsyncHook>) -> Result<()> {
    let bytes = manifest::encode(manifest)?;
    let name = manifest_name(manifest.manifest_version);
    if let Some(hook) = hook {
        hook.before(IoAction::Write {
            file: &name,
            offset: 0,
            len: bytes.len(),
        })?;
    }
    storage::write_atomic(root, &name, &bytes)?;
    if let Some(hook) = hook {
        hook.before(IoAction::Write {
            file: CURRENT_FILE,
            offset: 0,
            len: manifest.manifest_version.to_string().len(),
        })?;
    }
    storage::write_atomic(
        root,
        CURRENT_FILE,
        manifest.manifest_version.to_string().as_bytes(),
    )?;
    prune_manifests(root)?;
    Ok(())
}

/// 删除多余的历史 MANIFEST,只保留最近 `MANIFEST_KEEP` 个。
fn prune_manifests(root: &Path) -> Result<()> {
    let mut versions: Vec<(u64, String)> = storage::list_dir(root, "")?
        .into_iter()
        .filter_map(|name| parse_manifest_name(&name).map(|version| (version, name)))
        .collect();
    versions.sort_by_key(|(version, _)| *version);
    while versions.len() > MANIFEST_KEEP {
        let (_, name) = versions.remove(0);
        storage::remove_if_exists(&root.join(name))?;
    }
    Ok(())
}

/// 读取 MANIFEST 所列各段的字节。
fn read_segment_bytes(root: &Path, manifest: &Manifest) -> Result<Vec<SegmentBytes>> {
    let mut segments = Vec::new();
    for segment in &manifest.segments {
        let vsec = storage::read_file_opt(
            root,
            &format!("{SEGMENTS_DIR}/{}", vsec_name(segment.segment_id)),
        )?
        .unwrap_or_default();
        let msec = storage::read_file_opt(
            root,
            &format!("{SEGMENTS_DIR}/{}", msec_name(segment.segment_id)),
        )?
        .unwrap_or_default();
        if vsec.is_empty() && msec.is_empty() {
            continue;
        }
        segments.push(SegmentBytes {
            segment_id: segment.segment_id,
            vsec,
            msec,
        });
    }
    Ok(segments)
}
