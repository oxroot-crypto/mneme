//! `wal_writer` 模块的单元测试(文件序号解析、帧上限、重置与内存后端生命周期)。

use super::*;

use std::sync::Arc;

use crate::core::error::MnemeError;
use crate::core::metric::Metric;
use crate::core::options::FsyncPolicy;
use crate::persist::hook::{FsyncHook, IoAction};
use crate::persist::storage::Storage;
use crate::persist::wal;

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
        frame_max: crate::core::options::Limits::default().wal_frame_max,
    }
}

/// FC-PERSIST-ERR-013:单帧负载超上限 → `LimitExceeded` 且该帧不落盘;
/// 恰好等于上限仍可写入(边界三点:limit-1 由等于上限覆盖、limit、limit+1)。
#[test]
fn frame_payload_over_limit_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut overridden = config(None);
    overridden.frame_max = 64;
    let mut writer = WalWriter::open_or_create(fs(dir.path()), overridden).expect("create wal");
    writer
        .append(1, wal::FrameKind::DeleteRow, &[0_u8; 64])
        .expect("恰好等于上限必须可写");
    let before = writer.written;
    let result = writer.append(2, wal::FrameKind::DeleteRow, &[0_u8; 65]);
    assert!(
        matches!(
            result,
            Err(MnemeError::LimitExceeded {
                limit: 64,
                got: 65,
                ..
            })
        ),
        "超限必须报 LimitExceeded,实际 {result:?}"
    );
    assert_eq!(writer.written, before, "被拒绝的帧绝不落盘");
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
    let mut writer = WalWriter::open_or_create(fs(dir.path()), config(None)).expect("create wal");
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
