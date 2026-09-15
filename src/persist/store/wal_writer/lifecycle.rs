//! WAL 写入器的打开/新建与只读装配(`wal_writer/lifecycle.rs`)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::persist::hook::IoAction;
use crate::persist::storage::{Storage, WAL_DIR};
use crate::persist::wal;

use super::config::WalConfig;
use super::inputs::{FromPartsInput, OpenExistingInput};
use super::paths::{wal_files, wal_index_of, wal_name};
use super::writer::WalWriter;

impl WalWriter {
    /// 打开既有活动 WAL(追加)或新建;只读实例仅只读打开、不创建。
    ///
    /// 既有 WAL 的头部维度/度量与当前库不符时返回 `Corrupted`(MANIFEST 已锁定
    /// 身份,不符即数据损坏);头部截断(Checkpoint 中途崩溃)时原地重建,绝不
    /// 静默丢弃已 fsync 的完整帧。
    ///
    /// # Errors
    /// I/O 失败返回 [`MnemeError::Io`];身份不符返回 [`MnemeError::Corrupted`]。
    pub(in crate::persist::store) fn open_or_create(
        storage: Arc<dyn Storage>,
        config: WalConfig,
    ) -> Result<Self> {
        let files = wal_files(storage.as_ref())?;
        if config.read_only {
            return Ok(Self::read_only_writer(storage, &files, config));
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
            Some(_) => Self::open_existing(OpenExistingInput {
                storage,
                files: &files,
                rel,
                index,
                config,
            }),
            // 短头/损坏头(Checkpoint 中途崩溃):原地重建,后续帧本就不完整。
            None => create_truncating(storage, index, config),
        }
    }

    /// 只读实例不写入;总字节按当前文件集一次性统计(无活动句柄)。
    fn read_only_writer(storage: Arc<dyn Storage>, files: &[String], config: WalConfig) -> Self {
        let total: u64 = files
            .iter()
            .filter_map(|rel| storage.stat(rel).ok())
            .map(|meta| meta.len)
            .sum();
        let index = files
            .last()
            .and_then(|rel| wal_index_of(rel.rsplit('/').next().unwrap_or(rel)))
            .unwrap_or(0);
        Self::from_parts(FromPartsInput {
            storage,
            active_index: index,
            config,
            writable: false,
            written: total,
            sealed: 0,
        })
    }

    /// 打开既有活动 WAL:统计活动文件与已轮转旧文件字节。
    fn open_existing(input: OpenExistingInput<'_>) -> Result<Self> {
        let OpenExistingInput {
            storage,
            files,
            rel,
            index,
            config,
        } = input;
        let written = storage.stat(rel)?.len;
        let sealed: u64 = files
            .iter()
            .filter(|other| other.as_str() != rel)
            .filter_map(|other| storage.stat(other).ok())
            .map(|meta| meta.len)
            .sum();
        Ok(Self::from_parts(FromPartsInput {
            storage,
            active_index: index,
            config,
            writable: true,
            written,
            sealed,
        }))
    }
}

// `pub(super)` 供 `writer` 子模块的轮转路径复用;拆分不改变 `wal_writer` 外可达性。
/// 以截断方式新建/重建 WAL:原子写文件头。
pub(super) fn create_truncating(
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
    Ok(WalWriter::from_parts(FromPartsInput {
        storage,
        active_index: index,
        config,
        writable: true,
        written: wal::FILE_HEADER_LEN as u64,
        sealed: 0,
    }))
}
