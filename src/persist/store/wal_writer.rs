//! WAL 写入器与 WAL 文件集(`store/wal_writer.rs`)。
//!
//! 持有当前活动 WAL 文件句柄,按 [`FsyncPolicy`] 决定落盘时机;单文件达
//! `wal_file_bytes` 后在下一次事务提交时轮转到下一个文件(**单批不跨文件**,
//! 设计 04 §3.2)。只读实例不持有可写句柄(`file = None`);任何写方法返回
//! `Unsupported`,且打开时绝不创建或改写 WAL 文件(设计 12 §2 只读共享)。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::FsyncPolicy;
use crate::persist::hook::{FsyncHook, IoAction};
use crate::persist::storage::{self, WAL_DIR};
use crate::persist::wal;

/// 首文件相对路径(常量;轮转文件按序号递增)。
pub(super) const WAL_FILE: &str = "wal/wal_000001.log";

/// 活动 WAL 文件相对路径(按文件序号,`000001` 起)。
pub(super) fn wal_name(index: u32) -> String {
    format!("{WAL_DIR}/wal_{index:06}.log")
}

/// 列出全部 WAL 文件相对路径(按文件序号升序)。
///
/// # Errors
/// 目录列举失败时返回 [`MnemeError::Io`]。
pub(super) fn wal_files(root: &Path) -> Result<Vec<String>> {
    let mut names: Vec<(u32, String)> = storage::list_dir(root, WAL_DIR)?
        .into_iter()
        .filter_map(|name| {
            let index = wal_index_of(&name)?;
            Some((index, format!("{WAL_DIR}/{name}")))
        })
        .collect();
    names.sort_by_key(|(index, _)| *index);
    Ok(names.into_iter().map(|(_, rel)| rel).collect())
}

/// 从 `wal_<6 位序号>.log` 文件名解析序号;不匹配返回 `None`。
fn wal_index_of(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("wal_")?.strip_suffix(".log")?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u32>().ok()
}

/// 打开 WAL 写入器所需的身份与策略参数。
pub(super) struct WalConfig {
    /// 建库维度(写入 WAL 文件头)。
    pub(super) dimension: u32,
    /// 距离度量(写入 WAL 文件头)。
    pub(super) metric: Metric,
    /// fsync 策略。
    pub(super) policy: FsyncPolicy,
    /// 单文件轮转阈值(字节);`0` = 不轮转。
    pub(super) max_file_bytes: u64,
    /// 崩溃注入钩子。
    pub(super) hook: Option<Arc<dyn FsyncHook>>,
    /// 只读打开(仅只读句柄、不创建/改写)。
    pub(super) read_only: bool,
}

/// WAL 写入器(单活动文件;轮转后序号递增)。
pub(super) struct WalWriter {
    root: PathBuf,
    file: Option<std::fs::File>,
    active_index: u32,
    max_file_bytes: u64,
    policy: FsyncPolicy,
    dimension: u32,
    metric: Metric,
    hook: Option<Arc<dyn FsyncHook>>,
    /// 重置失败且重建失败后停用:后续写入报 `Io`,绝不向状态可疑的文件追加
    /// (否则重启截断会丢已确认帧)。
    poisoned: bool,
}

impl WalWriter {
    /// 打开既有活动 WAL(追加)或新建;只读实例仅只读打开、不创建。
    ///
    /// 既有 WAL 的头部维度/度量与当前库不符时返回 `Corrupted`(MANIFEST 已锁定
    /// 身份,不符即数据损坏);头部截断(Checkpoint 中途崩溃)时原地重建,绝不
    /// 静默丢弃已 fsync 的完整帧。
    ///
    /// # Errors
    /// I/O 失败返回 [`MnemeError::Io`];身份不符返回 [`MnemeError::Corrupted`]。
    pub(super) fn open_or_create(root: &Path, config: WalConfig) -> Result<Self> {
        let files = wal_files(root)?;
        if config.read_only {
            return open_read_only(root, &files, config);
        }
        open_writable(root, &files, config)
    }

    /// 由文件句柄与配置组装写入器。
    fn from_parts(
        root: PathBuf,
        file: Option<std::fs::File>,
        active_index: u32,
        config: WalConfig,
    ) -> Self {
        Self {
            root,
            file,
            active_index,
            max_file_bytes: config.max_file_bytes,
            policy: config.policy,
            dimension: config.dimension,
            metric: config.metric,
            hook: config.hook,
            poisoned: false,
        }
    }

    /// 当前活动文件相对路径。
    pub(super) fn active_rel(&self) -> String {
        wal_name(self.active_index)
    }

    /// 打开可写句柄;只读实例返回 `Unsupported`,已停用句柄返回 `Io`。
    fn writable(&mut self) -> Result<&mut std::fs::File> {
        if self.poisoned {
            return Err(MnemeError::Io(std::io::Error::other(
                "WAL 重置失败,句柄已停用",
            )));
        }
        self.file.as_mut().ok_or(MnemeError::Unsupported {
            feature: "只读模式写入",
        })
    }

    /// 追加一帧(不 fsync;由 [`WalWriter::sync`] 在事务末统一落盘)。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(super) fn append(
        &mut self,
        seqno: u64,
        kind: wal::FrameKind,
        payload: &[u8],
    ) -> Result<()> {
        use std::io::Write as _;
        let frame = wal::encode_frame(seqno, kind, payload);
        let active = self.active_rel();
        let hook = self.hook.clone();
        let file = self.writable()?;
        let offset = file.metadata()?.len();
        if let Some(hook) = &hook {
            hook.before(IoAction::Write {
                file: &active,
                offset,
                len: frame.len(),
            })?;
        }
        file.write_all(&frame)?;
        Ok(())
    }

    /// 按 `FsyncPolicy` 落盘:一个写事务只调用一次(组提交,FC-PERSIST-CPLX-001);
    /// 事务末检查是否需要轮转(单批绝不跨文件)。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];同步 I/O 失败返回 [`MnemeError::Io`]。
    pub(super) fn sync(&mut self) -> Result<()> {
        if matches!(self.policy, FsyncPolicy::Always | FsyncPolicy::Batched(_)) {
            let active = self.active_rel();
            let hook = self.hook.clone();
            let file = self.writable()?;
            if let Some(hook) = &hook {
                hook.before(IoAction::Fsync { file: &active })?;
            }
            file.sync_all()?;
        }
        self.rotate_if_needed()
    }

    /// 活动文件达到轮转阈值时,在下个事务开始前切换到新文件。
    fn rotate_if_needed(&mut self) -> Result<()> {
        if self.max_file_bytes == 0 || self.file.is_none() {
            return Ok(());
        }
        let Some(file) = &self.file else {
            return Ok(());
        };
        if file.metadata()?.len() < self.max_file_bytes {
            return Ok(());
        }
        let next = self.active_index.saturating_add(1);
        let path = storage::resolve(&self.root, &wal_name(next))?;
        create_truncating(
            &path,
            next,
            WalConfig {
                dimension: self.dimension,
                metric: self.metric,
                policy: self.policy,
                max_file_bytes: self.max_file_bytes,
                hook: self.hook.clone(),
                read_only: false,
            },
        )
        .map(|writer| {
            self.file = writer.file;
            self.active_index = next;
        })
    }

    /// 当前活动 WAL 文件字节长度(用于写事务失败时回滚截断点)。
    ///
    /// # Errors
    /// 元数据读取失败时返回 [`MnemeError::Io`]。
    pub(super) fn len(&self) -> Result<u64> {
        match &self.file {
            Some(file) => Ok(file.metadata()?.len()),
            None => Ok(0),
        }
    }

    /// 回滚到 `len`:截断活动文件并 seek 到末尾,丢弃本次事务的半写/未确认帧。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(super) fn rollback_to(&mut self, len: u64) -> Result<()> {
        let active = self.active_rel();
        let hook = self.hook.clone();
        let file = self.writable()?;
        file.set_len(len)?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(len))?;
        if let Some(hook) = &hook {
            hook.before(IoAction::Fsync { file: &active })?;
        }
        file.sync_all()?;
        Ok(())
    }

    /// 重置/Checkpoint:重写文件头、截断活动文件并删除已轮转旧文件。
    ///
    /// 调用方保证所有 `seqno ≤ manifest.watermark` 的帧已被段/delta 覆盖;
    /// 旧文件即使残留,恢复时也会因 `seqno ≤ watermark` 被跳过。
    ///
    /// 实现按"先写头、后截断"顺序,任何时刻文件头都完整;任一步失败时尝试原地
    /// 重建,重建仍失败才停用句柄——绝不向可疑文件继续追加(否则重启截断会丢
    /// 已确认帧,FC-PERSIST-INV-005)。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(super) fn reset(&mut self) -> Result<()> {
        if let Err(error) = self.rewrite_header_and_truncate() {
            if self.recreate().is_err() {
                self.poisoned = true;
            }
            return Err(error);
        }
        let active = self.active_rel();
        for rel in wal_files(&self.root)? {
            if rel != active {
                // reason: 旧轮转文件即使残留,恢复时也因 seqno ≤ watermark 被跳过,
                // 删除失败不影响正确性。
                let _ = storage::remove_if_exists(&storage::resolve(&self.root, &rel)?).ok();
            }
        }
        Ok(())
    }

    /// 重置的第一步:覆盖文件头并把文件截到头部长度。
    fn rewrite_header_and_truncate(&mut self) -> Result<()> {
        use std::io::{Seek, SeekFrom, Write as _};
        let active = self.active_rel();
        let header = wal::encode_file_header(self.dimension, self.metric);
        let hook = self.hook.clone();
        let file = self.writable()?;
        if let Some(hook) = &hook {
            hook.before(IoAction::Write {
                file: &active,
                offset: 0,
                len: header.len(),
            })?;
        }
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;
        file.set_len(wal::FILE_HEADER_LEN as u64)?;
        file.seek(SeekFrom::Start(wal::FILE_HEADER_LEN as u64))?;
        if let Some(hook) = &hook {
            hook.before(IoAction::Fsync { file: &active })?;
        }
        file.sync_all()?;
        Ok(())
    }

    /// 重置失败后的兜底:截断重建当前活动文件(写新头 + fsync)。
    fn recreate(&mut self) -> Result<()> {
        let path = storage::resolve(&self.root, &self.active_rel())?;
        let writer = create_truncating(
            &path,
            self.active_index,
            WalConfig {
                dimension: self.dimension,
                metric: self.metric,
                policy: self.policy,
                max_file_bytes: self.max_file_bytes,
                hook: self.hook.clone(),
                read_only: false,
            },
        )?;
        self.file = writer.file;
        Ok(())
    }
}

/// 只读打开:仅只读句柄、不创建文件;WAL 不存在时 `file = None`。
fn open_read_only(root: &Path, files: &[String], config: WalConfig) -> Result<WalWriter> {
    let file = match files.last() {
        Some(rel) => Some(
            std::fs::OpenOptions::new()
                .read(true)
                .open(storage::resolve(root, rel)?)?,
        ),
        None => None,
    };
    let active_index = files
        .last()
        .and_then(|rel| wal_index_of(rel.rsplit('/').next().unwrap_or(rel)))
        .unwrap_or(0);
    Ok(WalWriter::from_parts(
        root.to_path_buf(),
        file,
        active_index,
        config,
    ))
}

/// 可写打开:确保目录、校验/重建文件头并定位活动文件。
fn open_writable(root: &Path, files: &[String], config: WalConfig) -> Result<WalWriter> {
    storage::ensure_dir(&root.join(WAL_DIR))?;
    let Some(rel) = files.last() else {
        return create_truncating(&storage::resolve(root, WAL_FILE)?, 1, config);
    };
    let path = storage::resolve(root, rel)?;
    let index = wal_index_of(rel.rsplit('/').next().unwrap_or(rel)).unwrap_or(1);
    let bytes = storage::read_file(root, rel)?;
    let header = if bytes.len() >= wal::FILE_HEADER_LEN {
        wal::parse_file_header(&bytes).ok()
    } else {
        None
    };
    match header {
        Some(header) if header.dimension != config.dimension || header.metric != config.metric => {
            Err(MnemeError::Corrupted {
                segment: None,
                reason: "WAL 头维度/度量与 MANIFEST 不符".to_string(),
            })
        }
        Some(_) => {
            // 以 `write`(而非 `append`)打开:Windows 下 append-only 句柄缺少
            // FILE_WRITE_DATA,`set_len`(Checkpoint 重置)会被拒绝。
            let mut file = std::fs::OpenOptions::new().write(true).open(&path)?;
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::End(0))?;
            Ok(WalWriter::from_parts(
                root.to_path_buf(),
                Some(file),
                index,
                config,
            ))
        }
        // 短头/损坏头(Checkpoint 中途崩溃):原地重建,后续帧本就不完整。
        None => create_truncating(&path, index, config),
    }
}

/// 以截断方式新建/重建 WAL:写文件头并 fsync。
///
/// 名为「truncating」是因为实现用 `File::create` **覆盖**既有文件;调用场景
/// (首次创建、短头重建、重置兜底)都需要覆盖语义。
fn create_truncating(path: &Path, index: u32, config: WalConfig) -> Result<WalWriter> {
    let header = wal::encode_file_header(config.dimension, config.metric);
    let rel = wal_name(index);
    let root = path
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let mut file = std::fs::File::create(path)?;
    use std::io::Write as _;
    if let Some(hook) = &config.hook {
        hook.before(IoAction::Write {
            file: &rel,
            offset: 0,
            len: header.len(),
        })?;
    }
    file.write_all(&header)?;
    file.sync_all()?;
    Ok(WalWriter::from_parts(root, Some(file), index, config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_index_parsing_accepts_canonical_names_only() {
        assert_eq!(wal_index_of("wal_000001.log"), Some(1));
        assert_eq!(wal_index_of("wal_000042.log"), Some(42));
        assert_eq!(wal_index_of("wal_1.log"), None);
        assert_eq!(wal_index_of("wal_0000001.log"), None);
        assert_eq!(wal_index_of("seg_000001.log"), None);
        assert_eq!(wal_index_of("wal_00000a.log"), None);
    }

    /// 首次建文件放行,之后所有 WAL 头写入都拒绝。
    struct DenyAfterFirst {
        seen: std::sync::atomic::AtomicUsize,
    }

    impl FsyncHook for DenyAfterFirst {
        fn before(&self, action: IoAction<'_>) -> std::io::Result<()> {
            if let IoAction::Write {
                file, offset: 0, ..
            } = action
                && file.starts_with("wal/")
                && self.seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 1
            {
                return Err(std::io::Error::other("injected wal header failure"));
            }
            Ok(())
        }
    }

    fn config(hook: Option<Arc<dyn FsyncHook>>) -> WalConfig {
        WalConfig {
            dimension: 4,
            metric: Metric::Cosine,
            policy: FsyncPolicy::default(),
            max_file_bytes: 0,
            hook,
            read_only: false,
        }
    }

    /// FC-PERSIST-INV-005:重置失败且重建失败时停用句柄,绝不向状态可疑的文件追加。
    #[test]
    fn failed_reset_poisons_writer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hook = Arc::new(DenyAfterFirst {
            seen: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut writer =
            WalWriter::open_or_create(dir.path(), config(Some(hook))).expect("create wal");
        assert!(writer.reset().is_err(), "注入的头写入失败必须上报");
        let result = writer.append(1, wal::FrameKind::DeleteRow, &[]);
        assert!(
            matches!(result, Err(MnemeError::Io(_))),
            "停用后写入必须报 Io,实际 {result:?}"
        );
    }

    /// FC-PERSIST-POST-002:成功重置后文件只剩完整文件头(先写头、后截断)。
    #[test]
    fn reset_keeps_complete_header() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut writer = WalWriter::open_or_create(dir.path(), config(None)).expect("create wal");
        writer
            .append(1, wal::FrameKind::DeleteRow, &[])
            .expect("append");
        writer.reset().expect("reset");
        let bytes = std::fs::read(dir.path().join("wal/wal_000001.log")).expect("read wal");
        assert_eq!(bytes.len(), wal::FILE_HEADER_LEN);
        let header = wal::parse_file_header(&bytes).expect("reset 后头必须完整可解析");
        assert_eq!(header.dimension, 4);
        assert_eq!(header.metric, Metric::Cosine);
    }
}
