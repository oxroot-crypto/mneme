//! WAL 写入器(`store/wal_writer.rs`)。
//!
//! 持有当前 WAL 文件句柄,按 [`FsyncPolicy`] 决定落盘时机。只读实例不持有可写
//! 句柄(`file = None`);任何写方法返回 `Unsupported`,且打开时绝不创建或改写
//! WAL 文件(设计 12 §2 只读共享)。

use std::path::Path;
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::FsyncPolicy;
use crate::persist::hook::{FsyncHook, IoAction};
use crate::persist::storage::{self, WAL_DIR};
use crate::persist::wal;

/// 单个 WAL 文件名(全量快照模式下只需一个,Checkpoint 时重建)。
pub(super) const WAL_FILE: &str = "wal/wal_000001.log";

/// 打开 WAL 写入器所需的身份与策略参数。
pub(super) struct WalConfig {
    /// 建库维度(写入 WAL 文件头)。
    pub(super) dimension: u32,
    /// 距离度量(写入 WAL 文件头)。
    pub(super) metric: Metric,
    /// fsync 策略。
    pub(super) policy: FsyncPolicy,
    /// 崩溃注入钩子。
    pub(super) hook: Option<Arc<dyn FsyncHook>>,
    /// 只读打开(仅只读句柄、不创建/改写)。
    pub(super) read_only: bool,
}

/// WAL 写入器。
pub(super) struct WalWriter {
    file: Option<std::fs::File>,
    policy: FsyncPolicy,
    dimension: u32,
    metric: Metric,
    hook: Option<Arc<dyn FsyncHook>>,
}

impl WalWriter {
    /// 打开既有 WAL(追加)或新建(写文件头);只读实例仅只读打开既有 WAL、不创建。
    ///
    /// 既有 WAL 的头部维度/度量与当前库不符(或头部损坏)时重建(仅可写实例);
    /// 否则保留既有帧,由 `Store::open` 回放后再继续追加——绝不在此截断已 fsync 的帧。
    ///
    /// # Errors
    /// I/O 失败返回 [`MnemeError::Io`]。
    pub(super) fn open_or_create(root: &Path, config: WalConfig) -> Result<Self> {
        let path = storage::resolve(root, WAL_FILE)?;
        if config.read_only {
            // 只读实例不建目录、不创建文件;WAL 不存在时仅不持有句柄。
            return open_read_only(path, config);
        }
        storage::ensure_dir(&root.join(WAL_DIR))?;
        if let Some(file) = reuse_existing(root, &path, config.dimension, config.metric)? {
            return Ok(Self::from_parts(Some(file), config));
        }
        create_new(&path, config)
    }

    /// 由文件句柄与配置组装写入器。
    fn from_parts(file: Option<std::fs::File>, config: WalConfig) -> Self {
        Self {
            file,
            policy: config.policy,
            dimension: config.dimension,
            metric: config.metric,
            hook: config.hook,
        }
    }

    /// 打开可写句柄;只读实例返回 `Unsupported`。
    fn writable(&mut self) -> Result<&mut std::fs::File> {
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
        let hook = self.hook.clone();
        let file = self.writable()?;
        let offset = file.metadata()?.len();
        if let Some(hook) = &hook {
            hook.before(IoAction::Write {
                file: WAL_FILE,
                offset,
                len: frame.len(),
            })?;
        }
        file.write_all(&frame)?;
        Ok(())
    }

    /// 按 `FsyncPolicy` 落盘:一个写事务只调用一次(组提交,FC-PERSIST-CPLX-001)。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];同步 I/O 失败返回 [`MnemeError::Io`]。
    pub(super) fn sync(&mut self) -> Result<()> {
        if !matches!(self.policy, FsyncPolicy::Always | FsyncPolicy::Batched(_)) {
            return Ok(());
        }
        let hook = self.hook.clone();
        let file = self.writable()?;
        if let Some(hook) = &hook {
            hook.before(IoAction::Fsync { file: WAL_FILE })?;
        }
        file.sync_all()?;
        Ok(())
    }

    /// 当前 WAL 文件字节长度(用于写事务失败时回滚截断点)。
    ///
    /// # Errors
    /// 元数据读取失败时返回 [`MnemeError::Io`]。
    pub(super) fn len(&self) -> Result<u64> {
        match &self.file {
            Some(file) => Ok(file.metadata()?.len()),
            None => Ok(0),
        }
    }

    /// 回滚到 `len`:截断文件并 seek 到末尾,丢弃本次事务已写入的半写/未确认帧。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(super) fn rollback_to(&mut self, len: u64) -> Result<()> {
        let file = self.writable()?;
        file.set_len(len)?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(len))?;
        file.sync_all()?;
        Ok(())
    }

    /// 重置 WAL(Checkpoint):截断为空并重写文件头。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(super) fn reset(&mut self) -> Result<()> {
        let (dimension, metric) = (self.dimension, self.metric);
        let file = self.writable()?;
        file.set_len(0)?;
        use std::io::{Seek, SeekFrom, Write as _};
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&wal::encode_file_header(dimension, metric))?;
        file.sync_all()?;
        Ok(())
    }
}

/// 只读打开:若 WAL 存在则只读打开(不用于写入),否则不持有句柄。
fn open_read_only(path: std::path::PathBuf, config: WalConfig) -> Result<WalWriter> {
    let file = if storage::exists(&path)? {
        Some(std::fs::OpenOptions::new().read(true).open(&path)?)
    } else {
        None
    };
    Ok(WalWriter::from_parts(file, config))
}

/// 复用维度/度量匹配的既有 WAL;不匹配时返回 `None`。
fn reuse_existing(
    root: &Path,
    path: &Path,
    dimension: u32,
    metric: Metric,
) -> Result<Option<std::fs::File>> {
    let reuse = storage::read_file_opt(root, WAL_FILE)?
        .filter(|bytes| bytes.len() >= wal::FILE_HEADER_LEN)
        .and_then(|bytes| wal::parse_file_header(&bytes).ok())
        .is_some_and(|header| header.dimension == dimension && header.metric == metric);
    if !reuse {
        return Ok(None);
    }
    // 以 `write`(而非 `append`)打开:Windows 下 append-only 句柄缺少
    // FILE_WRITE_DATA,`set_len`(Checkpoint 重置)会被拒绝。
    let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::End(0))?;
    Ok(Some(file))
}

/// 新建 WAL:写文件头并 fsync。
fn create_new(path: &Path, config: WalConfig) -> Result<WalWriter> {
    let header = wal::encode_file_header(config.dimension, config.metric);
    let mut file = std::fs::File::create(path)?;
    use std::io::Write as _;
    if let Some(hook) = &config.hook {
        hook.before(IoAction::Write {
            file: WAL_FILE,
            offset: 0,
            len: header.len(),
        })?;
    }
    file.write_all(&header)?;
    file.sync_all()?;
    Ok(WalWriter::from_parts(Some(file), config))
}
