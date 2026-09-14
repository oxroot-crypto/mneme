//! WAL 写入器与 WAL 文件集(`store/wal_writer.rs`)。
//!
//! 持有当前活动 WAL **相对路径**,经 [`Storage`] 后端追加/同步/轮转;按
//! [`FsyncPolicy`] 决定落盘时机,单文件达 `wal_file_bytes` 后在下一次事务提交时
//! 轮转到下一个文件(**单批不跨文件**,设计 04 §3.2)。只读实例不写入
//! (`writable = false`);任何写方法返回 `Unsupported`,且打开时绝不创建或改写
//! WAL 文件(设计 12 §2 只读共享)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::FsyncPolicy;
use crate::persist::hook::{FsyncHook, IoAction};
use crate::persist::storage::{Storage, WAL_DIR};
use crate::persist::wal;

/// 活动 WAL 文件相对路径(按文件序号,`000001` 起)。
pub(super) fn wal_name(index: u32) -> String {
    format!("{WAL_DIR}/wal_{index:06}.log")
}

/// 列出全部 WAL 文件相对路径(按文件序号升序)。
///
/// # Errors
/// 目录列举失败时返回 [`MnemeError::Io`]。
pub(super) fn wal_files(storage: &dyn Storage) -> Result<Vec<String>> {
    let mut names: Vec<String> = storage
        .list_dir(WAL_DIR)?
        .into_iter()
        .filter(|name| name.starts_with("wal_") && name.ends_with(".log"))
        .map(|name| format!("{WAL_DIR}/{name}"))
        .collect();
    names.sort();
    Ok(names)
}

/// 从 WAL 文件名解析文件序号(严格 `wal_NNNNNN.log`;不匹配返回 `None`)。
pub(super) fn wal_index_of(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("wal_")?.strip_suffix(".log")?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// WAL 打开参数(维度/度量来自 MANIFEST,建库即锁定)。
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
    /// 静态加密配置(`None` = 明文帧)。
    pub(super) encryption: Option<crate::crypto::Encryption>,
}

/// WAL 写入器(单活动文件;轮转后序号递增;字节操作全部经 [`Storage`])。
pub(super) struct WalWriter {
    storage: Arc<dyn Storage>,
    active_index: u32,
    /// 活动文件相对路径(轮转/重建时同步更新,免每次追加都重新格式化)。
    active_rel: String,
    /// 活动文件当前字节数(本写入器是唯一写者;追加/回滚/重建处同步维护)。
    written: u64,
    /// 已轮转旧文件的字节数合计(轮转时累加、Checkpoint 删除旧文件后清零)。
    ///
    /// 供 `Store::wal_bytes` 免列目录 + stat 直接取总量(统计为尽力而为)。
    sealed_bytes: u64,
    max_file_bytes: u64,
    policy: FsyncPolicy,
    dimension: u32,
    metric: Metric,
    hook: Option<Arc<dyn FsyncHook>>,
    encryption: Option<crate::crypto::Encryption>,
    /// 只读实例(无写权限)。
    writable: bool,
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
    pub(super) fn open_or_create(storage: Arc<dyn Storage>, config: WalConfig) -> Result<Self> {
        let files = wal_files(storage.as_ref())?;
        if config.read_only {
            // 只读实例不写入;总字节按当前文件集一次性统计(无活动句柄)。
            let total: u64 = files
                .iter()
                .filter_map(|rel| storage.stat(rel).ok())
                .map(|meta| meta.len)
                .sum();
            return Ok(Self::from_parts(
                storage,
                files
                    .last()
                    .and_then(|rel| wal_index_of(rel.rsplit('/').next().unwrap_or(rel)))
                    .unwrap_or(0),
                config,
                false,
                total,
                0,
            ));
        }
        storage.ensure_dir(WAL_DIR)?;
        let Some(rel) = files.last() else {
            return create_truncating(storage, 1, config);
        };
        let index = wal_index_of(rel.rsplit('/').next().unwrap_or(rel)).unwrap_or(1);
        let bytes = storage.read_file(rel)?;
        let header = if bytes.len() >= wal::FILE_HEADER_LEN {
            wal::parse_file_header(&bytes).ok()
        } else {
            None
        };
        match header {
            Some(header)
                if header.dimension != config.dimension || header.metric != config.metric =>
            {
                Err(MnemeError::Corrupted {
                    segment: None,
                    reason: "WAL 头维度/度量与 MANIFEST 不符".to_string(),
                })
            }
            Some(_) => {
                let written = storage.stat(rel)?.len;
                let sealed: u64 = files
                    .iter()
                    .filter(|other| other.as_str() != rel.as_str())
                    .filter_map(|other| storage.stat(other).ok())
                    .map(|meta| meta.len)
                    .sum();
                Ok(Self::from_parts(
                    storage, index, config, true, written, sealed,
                ))
            }
            // 短头/损坏头(Checkpoint 中途崩溃):原地重建,后续帧本就不完整。
            None => create_truncating(storage, index, config),
        }
    }

    /// 由后端与配置组装写入器;`written` 为活动文件当前字节数,
    /// `sealed` 为已轮转旧文件字节合计。
    fn from_parts(
        storage: Arc<dyn Storage>,
        active_index: u32,
        config: WalConfig,
        writable: bool,
        written: u64,
        sealed: u64,
    ) -> Self {
        Self {
            storage,
            active_index,
            active_rel: wal_name(active_index),
            written,
            sealed_bytes: sealed,
            max_file_bytes: config.max_file_bytes,
            policy: config.policy,
            dimension: config.dimension,
            metric: config.metric,
            hook: config.hook,
            encryption: config.encryption,
            writable,
            poisoned: false,
        }
    }

    /// 当前活动文件相对路径。
    pub(super) fn active_rel(&self) -> &str {
        &self.active_rel
    }

    /// 写前置检查:只读 → `Unsupported`;已停用 → `Io`。
    fn ensure_writable(&self) -> Result<()> {
        if self.poisoned {
            return Err(MnemeError::Io(std::io::Error::other(
                "WAL 重置失败,句柄已停用",
            )));
        }
        if !self.writable {
            return Err(MnemeError::Unsupported {
                feature: "只读模式写入",
            });
        }
        Ok(())
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
        self.ensure_writable()?;
        let sealed = match self.encryption.as_ref() {
            Some(encryption) => Some(crate::crypto::seal(encryption, b"wal", seqno, payload)?),
            None => None,
        };
        let frame = wal::encode_frame(seqno, kind, sealed.as_deref().unwrap_or(payload));
        if let Some(hook) = &self.hook {
            hook.before(IoAction::Write {
                file: &self.active_rel,
                offset: self.written,
                len: frame.len(),
            })?;
        }
        self.written = self.storage.append(&self.active_rel, &frame)?;
        Ok(())
    }

    /// 按 `FsyncPolicy` 落盘:一个写事务只调用一次(组提交,FC-PERSIST-CPLX-001);
    /// 事务末检查是否需要轮转(单批绝不跨文件)。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];同步 I/O 失败返回 [`MnemeError::Io`]。
    pub(super) fn sync(&mut self) -> Result<()> {
        if self.writable && matches!(self.policy, FsyncPolicy::Always | FsyncPolicy::Batched(_)) {
            if let Some(hook) = &self.hook {
                hook.before(IoAction::Fsync {
                    file: &self.active_rel,
                })?;
            }
            self.storage.sync(&self.active_rel)?;
        }
        self.rotate_if_needed()
    }

    /// 活动文件达到轮转阈值时,在下个事务开始前切换到新文件。
    fn rotate_if_needed(&mut self) -> Result<()> {
        if self.max_file_bytes == 0 || !self.writable {
            return Ok(());
        }
        if self.written < self.max_file_bytes {
            return Ok(());
        }
        let next = self.active_index.saturating_add(1);
        let writer = create_truncating(
            Arc::clone(&self.storage),
            next,
            WalConfig {
                dimension: self.dimension,
                metric: self.metric,
                policy: self.policy,
                max_file_bytes: self.max_file_bytes,
                hook: self.hook.clone(),
                read_only: false,
                encryption: None,
            },
        )?;
        // 轮转:旧活动文件计入已封印字节,新写入器从文件头开始。
        let sealed = self.sealed_bytes.saturating_add(self.written);
        *self = writer;
        self.sealed_bytes = sealed;
        Ok(())
    }

    /// 当前 WAL 文件集总字节数(活动 + 已轮转;统计为尽力而为)。
    pub(super) fn total_bytes(&self) -> u64 {
        self.sealed_bytes.saturating_add(self.written)
    }

    /// 当前活动 WAL 文件字节长度(用于写事务失败时回滚截断点)。
    ///
    /// 缓存值由写入器在追加/回滚/重建处同步维护;本写入器是活动文件的唯一写者。
    ///
    /// # Errors
    /// 保留 `Result` 形态与读写失败调用点兼容;当前实现不产生错误。
    pub(super) fn len(&self) -> Result<u64> {
        Ok(self.written)
    }

    /// 回滚到 `len`:截断活动文件,丢弃本次事务的半写/未确认帧。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(super) fn rollback_to(&mut self, len: u64) -> Result<()> {
        self.ensure_writable()?;
        self.storage.truncate(&self.active_rel, len)?;
        self.written = len;
        if let Some(hook) = &self.hook {
            hook.before(IoAction::Fsync {
                file: &self.active_rel,
            })?;
        }
        self.storage.sync(&self.active_rel)?;
        Ok(())
    }

    /// 重置/Checkpoint:重写文件头、截断活动文件并删除已轮转旧文件。
    ///
    /// 调用方保证所有 `seqno ≤ manifest.watermark` 的帧已被段/delta 覆盖;
    /// 旧文件即使残留,恢复时也会因 `seqno ≤ watermark` 被跳过。
    ///
    /// 实现按"先原子换头、后截断"顺序,任何时刻文件头都完整;任一步失败时尝试原地
    /// 重建,重建仍失败才停用句柄——绝不向可疑文件追加(否则重启截断会丢已确认帧,
    /// FC-PERSIST-INV-005)。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(super) fn reset(&mut self) -> Result<()> {
        if let Err(error) = self.rewrite_header_and_truncate() {
            // 重建兜底:同样经过 hook(注入失败必须让 writer 停用,绝不向可疑文件追加)。
            let header = wal::encode_file_header(self.dimension, self.metric);
            let rebuild = (|| -> Result<()> {
                if let Some(hook) = &self.hook {
                    hook.before(IoAction::Write {
                        file: &self.active_rel,
                        offset: 0,
                        len: header.len(),
                    })?;
                }
                self.storage.write_atomic(&self.active_rel, &header)?;
                if let Some(hook) = &self.hook {
                    hook.before(IoAction::Fsync {
                        file: &self.active_rel,
                    })?;
                }
                self.storage.sync(&self.active_rel)?;
                Ok(())
            })();
            if rebuild.is_ok() {
                self.written = header.len() as u64;
            } else {
                self.poisoned = true;
            }
            return Err(error);
        }
        let active = self.active_rel();
        for rel in wal_files(self.storage.as_ref())? {
            if rel != active {
                // reason: 旧轮转文件即使残留,恢复时也因 seqno ≤ watermark 被跳过,
                // 删除失败不影响正确性。
                let _ = self.storage.remove_if_exists(&rel).ok();
            }
        }
        // Checkpoint 成功:旧轮转文件的字节不再计入 WAL 总量。
        self.sealed_bytes = 0;
        Ok(())
    }

    /// 重置的第一步:原子换头(写头 + fsync)并把活动文件截到头部长度。
    fn rewrite_header_and_truncate(&mut self) -> Result<()> {
        self.ensure_writable()?;
        let header = wal::encode_file_header(self.dimension, self.metric);
        if let Some(hook) = &self.hook {
            hook.before(IoAction::Write {
                file: &self.active_rel,
                offset: 0,
                len: header.len(),
            })?;
        }
        self.storage.write_atomic(&self.active_rel, &header)?;
        self.storage
            .truncate(&self.active_rel, wal::FILE_HEADER_LEN as u64)?;
        self.written = wal::FILE_HEADER_LEN as u64;
        if let Some(hook) = &self.hook {
            hook.before(IoAction::Fsync {
                file: &self.active_rel,
            })?;
        }
        self.storage.sync(&self.active_rel)?;
        Ok(())
    }
}

/// 以截断方式新建/重建 WAL:原子写文件头。
fn create_truncating(
    storage: Arc<dyn Storage>,
    index: u32,
    config: WalConfig,
) -> Result<WalWriter> {
    let header = wal::encode_file_header(config.dimension, config.metric);
    let rel = wal_name(index);
    if let Some(hook) = &config.hook {
        hook.before(IoAction::Write {
            file: &rel,
            offset: 0,
            len: header.len(),
        })?;
    }
    // 覆盖语义:write_atomic 先写临时文件再 rename,永远不会读到半截头。
    storage.write_atomic(&rel, &header)?;
    storage.sync(&rel)?;
    Ok(WalWriter::from_parts(
        storage,
        index,
        config,
        true,
        wal::FILE_HEADER_LEN as u64,
        0,
    ))
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
            encryption: None,
        }
    }

    fn fs(dir: &std::path::Path) -> Arc<dyn Storage> {
        Arc::new(crate::persist::storage::FsStorage::new(dir))
    }

    /// FC-PERSIST-INV-005:重置失败且重建失败时停用句柄,绝不向状态可疑的文件追加。
    #[test]
    fn failed_reset_poisons_writer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hook = Arc::new(DenyAfterFirst {
            seen: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut writer =
            WalWriter::open_or_create(fs(dir.path()), config(Some(hook))).expect("create wal");
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
        let mut writer =
            WalWriter::open_or_create(fs(dir.path()), config(None)).expect("create wal");
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

    /// 内存后端可完整走 WAL 追加/同步/重置(无 mmap 依赖;设计 12 §3.1)。
    #[test]
    fn mem_storage_supports_wal_lifecycle() {
        let storage: Arc<dyn Storage> = Arc::new(crate::persist::storage::MemStorage::new());
        let mut writer =
            WalWriter::open_or_create(Arc::clone(&storage), config(None)).expect("create wal");
        writer
            .append(7, wal::FrameKind::DeleteRow, b"payload")
            .expect("append");
        writer.sync().expect("sync");
        let bytes = storage
            .read_file("wal/wal_000001.log")
            .expect("read mem wal");
        assert!(bytes.len() > wal::FILE_HEADER_LEN);
        writer.reset().expect("reset");
        let bytes = storage
            .read_file("wal/wal_000001.log")
            .expect("read mem wal");
        assert_eq!(bytes.len(), wal::FILE_HEADER_LEN);
    }
}
