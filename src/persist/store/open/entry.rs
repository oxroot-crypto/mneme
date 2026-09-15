//! 打开/重载入口:目录布局、独占锁、WAL 配置准备与 [`Store::open`]。

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::FsyncPolicy;
use crate::memory::table::WriterState;
use crate::persist::manifest::Manifest;
use crate::persist::storage::{SEGMENTS_DIR, WAL_DIR};
use crate::persist::store::manifest_io;
use crate::persist::store::wal_writer::{WalConfig, WalWriter};
use crate::persist::store::{ManifestState, Store};
use crate::persist::trash;

use super::manifest_init::load_or_init_manifest;
use super::options::OpenOptions;
use super::state_load::load_write_state;

impl Store {
    /// 打开或新建持久库。
    ///
    /// 返回 `(store, 初始写状态, 维度, 度量)`。已存在库以 MANIFEST 的维度/度量为准,
    /// 与调用方请求不符时拒绝打开(设计 16 §3)。
    ///
    /// # Errors
    /// 目录不可用、锁被占、MANIFEST/段损坏(且 fail-fast)或维度冲突时返回结构化错误。
    pub(crate) fn open(
        root: &Path,
        options: OpenOptions,
    ) -> Result<(Arc<Store>, WriterState, u32, Metric)> {
        let storage = resolve_storage(root, options.storage.clone());
        prepare_root(storage.as_ref(), options.read_only)?;
        let lock = acquire_lock(&storage, options.read_only)?;
        // 清理崩溃残留的 `Building` 半成品(ATOMIC 写的 `.tmp`);只读实例不删除。
        cleanup_residue(storage.as_ref(), options.read_only)?;

        let (manifest, version) = load_or_init_manifest(&storage, &options)?;

        // 可写实例清理 MANIFEST 未引用的段孤儿(garbage),只读实例不写盘。
        if !options.read_only {
            manifest_io::remove_unreferenced_segments(storage.as_ref(), &manifest)?;
        }

        let state = load_write_state(&manifest, &options, &storage)?;
        let wal = WalWriter::open_or_create(Arc::clone(&storage), wal_config(&manifest, &options))?;
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
            read_only: options.read_only,
            hook: options.hook,
            index_factory: options.index_factory,
            compression: options.compression,
            encryption: options.encryption,
            storage,
            observer: options.observer,
            tuning: options.tuning.clone(),
            limits: options.limits,
        });
        Ok((store, state, manifest.dimension, manifest.metric))
    }
}

/// 解析存储后端:优先调用方注入,否则默认 [`FsStorage`](设计 12 §3.1)。
fn resolve_storage(
    root: &Path,
    requested: Option<Arc<dyn crate::persist::storage::Storage>>,
) -> Arc<dyn crate::persist::storage::Storage> {
    requested.unwrap_or_else(|| {
        Arc::new(crate::persist::storage::FsStorage::new(root))
            as Arc<dyn crate::persist::storage::Storage>
    })
}

/// 只读实例绝不写盘:只要求库目录已存在;可写实例建立目录布局。
fn prepare_root(storage: &dyn crate::persist::storage::Storage, read_only: bool) -> Result<()> {
    if !read_only {
        return prepare_dirs(storage);
    }
    if !storage.root_exists()? {
        return Err(MnemeError::Config {
            reason: "只读模式要求库目录已存在",
        });
    }
    Ok(())
}

/// 清理崩溃残留的 `Building` 半成品(ATOMIC 写的 `.tmp`);只读实例只读不删除。
fn cleanup_residue(storage: &dyn crate::persist::storage::Storage, read_only: bool) -> Result<()> {
    if read_only {
        return Ok(());
    }
    trash::purge(storage)?;
    manifest_io::cleanup_orphans(storage)?;
    Ok(())
}

/// 确保库根、`segments/`、`wal/` 与 `trash/` 目录存在。
fn prepare_dirs(storage: &dyn crate::persist::storage::Storage) -> Result<()> {
    storage.ensure_dir("")?;
    storage.ensure_dir(SEGMENTS_DIR)?;
    storage.ensure_dir(WAL_DIR)?;
    storage.ensure_dir(crate::persist::storage::TRASH_DIR)?;
    Ok(())
}

/// 可写实例取独占锁;只读实例不持锁。
fn acquire_lock(
    storage: &Arc<dyn crate::persist::storage::Storage>,
    read_only: bool,
) -> Result<Option<Box<dyn std::any::Any + Send + Sync>>> {
    if read_only {
        Ok(None)
    } else {
        Ok(Some(storage.try_lock()?))
    }
}

/// 由 MANIFEST 与打开参数构造 WAL 写入器配置。
fn wal_config(manifest: &Manifest, options: &OpenOptions) -> WalConfig {
    WalConfig {
        dimension: manifest.dimension,
        metric: manifest.metric,
        policy: options.fsync,
        max_file_bytes: options.wal_file_bytes,
        hook: options.hook.clone(),
        read_only: options.read_only,
        encryption: options.encryption.clone(),
        frame_max: options.limits.wal_frame_max,
    }
}

/// 只读实例重载:发现更新的已提交 MANIFEST 时重建写状态快照。
///
/// 返回 `Some((state, version))` 表示切换(`current` 或 MANIFEST 版本更新);
/// 无新版本返回 `None`。调用方以 `Table::publish` 原子换视图,旧视图由 `Arc`
/// 自然退役(I29/FC-DEPLOY-STA-001)。
///
/// # Errors
/// 新 MANIFEST/段损坏时返回结构化错误(只读实例保持旧视图,不中断服务)。
pub(crate) fn reload_read_only(store: &Store) -> Result<Option<(WriterState, u64)>> {
    if !store.read_only {
        return Err(MnemeError::Config {
            reason: "仅只读实例支持 reload",
        });
    }
    let Some(observed) = store.read_current() else {
        return Ok(None);
    };
    if observed <= store.current_version() {
        return Ok(None);
    }
    let Some((manifest, version)) =
        manifest_io::load_manifest(store.storage.as_ref(), store.encryption.as_ref())?
    else {
        return Ok(None);
    };
    if version <= store.current_version() {
        return Ok(None);
    }
    let options = OpenOptions {
        dimension: None,
        metric: None,
        fsync: FsyncPolicy::Never,
        read_only: true,
        verify_on_open: false,
        fail_fast_on_corruption: false,
        hook: None,
        index_factory: store.index_factory.clone(),
        wal_file_bytes: 0,
        tuning: store.tuning.clone(),
        compression: store.compression,
        encryption: store.encryption.clone(),
        storage: Some(Arc::clone(&store.storage)),
        observer: store.observer.clone(),
        limits: store.limits,
    };
    let state = load_write_state(&manifest, &options, &store.storage)?;
    store.replace_manifest(manifest, version);
    Ok(Some((state, version)))
}
