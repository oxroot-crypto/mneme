//! MANIFEST 载入 / 提交 / 裁剪与段文件清理(`store/manifest_io.rs`)。
//!
//! 这些是 [`Store`](super::Store) 打开与 flush 的纯文件系统辅助:不含
//! 业务状态,只负责目录扫描、原子提交与保留最近 `MANIFEST_KEEP` 版。

use std::collections::HashSet;
use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::persist::hook::{FsyncHook, IoAction};
use crate::persist::manifest::{self, Manifest};
use crate::persist::recover::SegmentBytes;
use crate::persist::source::{SegmentHandle, SegmentHandleOpenInput};
use crate::persist::storage::{
    CURRENT_FILE, MANIFEST_KEEP, SEGMENTS_DIR, Storage, WAL_DIR, hidx_name, manifest_name,
    msec_name, parse_manifest_name, vsec_name,
};

/// 清理 `segments/`、`wal/` 与根目录下的 `.tmp` 半成品(崩溃点 `Building` 孤儿)。
pub(super) fn cleanup_orphans(storage: &dyn Storage) -> Result<()> {
    for dir in [SEGMENTS_DIR, WAL_DIR, ""] {
        for name in storage.list_dir(dir)? {
            if !name.ends_with(".tmp") {
                continue;
            }
            let rel = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            storage.remove_if_exists(&rel)?;
        }
    }
    Ok(())
}

/// 读取 `current` 或扫描目录得到最新合法 MANIFEST。
///
/// 返回 `Ok(None)` 仅表示"全新库"(`current`/`MANIFEST.*` 均不存在)。
/// 若 `current` 存在但 `current` 指向的 MANIFEST 与目录内其余 `MANIFEST.*`
/// 全部非法,返回 [`MnemeError::Corrupted`]——**绝不**当作新库覆盖(设计 16 §3)。
/// 加密库在未开 `encrypt` 的构建上打开时,信封探测的 [`MnemeError::Unsupported`]
/// 必须原样上抛(`FC-SEC-ERR-001`):缺 feature 属全局配置错误,不得归入
/// 「无合法 MANIFEST」的 `Corrupted` 而丢失根因。
pub(super) fn load_manifest(
    storage: &dyn Storage,
    encryption: Option<&crate::crypto::Encryption>,
) -> Result<Option<(Manifest, u64)>> {
    let has_current = storage.exists(CURRENT_FILE)?;
    if let Some(bytes) = storage.read_file_opt(CURRENT_FILE)? {
        let text = String::from_utf8_lossy(&bytes);
        if let Ok(version) = text.trim().parse::<u64>()
            && let Some(manifest) =
                try_load_version(storage, encryption, &manifest_name(version), version)?
        {
            return Ok(Some((manifest, version)));
        }
    }
    // 回退:扫描目录取最大的、CRC 合法的版本。用目录里的实际文件名读取
    // (解析允许前导零等非规范写法,重建名会读空)。
    let mut best: Option<(Manifest, u64)> = None;
    for name in storage.list_dir("")? {
        let Some(version) = parse_manifest_name(&name) else {
            continue;
        };
        if let Some(manifest) = try_load_version(storage, encryption, &name, version)?
            && best
                .as_ref()
                .is_none_or(|(_, best_version)| version > *best_version)
        {
            best = Some((manifest, version));
        }
    }
    if best.is_none() && has_current {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "current 存在但无任何 CRC 合法的 MANIFEST".to_string(),
        });
    }
    Ok(best)
}

/// 尝试载入单个版本:`Ok(None)` = 该版本不存在或数据非法,可继续回退扫描。
///
/// # Errors
/// 信封探测遇未开 `encrypt` 的构建/未配置密钥 → [`MnemeError::Unsupported`]
/// 原样上抛(绝不归入 `Corrupted`,见 `FC-SEC-ERR-001`);读取 I/O 失败透传。
fn try_load_version(
    storage: &dyn Storage,
    encryption: Option<&crate::crypto::Encryption>,
    name: &str,
    version: u64,
) -> Result<Option<Manifest>> {
    let Some(bytes) = storage.read_file_opt(name)? else {
        return Ok(None);
    };
    let plain = match crate::crypto::decrypt_file(encryption, b"manifest", version, bytes) {
        Ok(plain) => plain,
        // reason: 缺 `encrypt` 能力或库未配置密钥时所有 MANIFEST 都解不开,属全局
        // 配置错误;必须上抛根因,不得按「版本非法」静默回退(FC-SEC-ERR-001)。
        Err(error @ MnemeError::Unsupported { .. }) => return Err(error),
        // reason: 单个版本解密/认证失败可能是密钥轮换残档或该文件损坏;保留
        // 「扫描其他合法版本」的回退语义,全部失败才报 `Corrupted`。
        Err(_) => return Ok(None),
    };
    Ok(manifest::parse(&plain).ok())
}

/// 列出 `segments/` 下的段文件(`.vsec`/`.msec`/`.hidx`)。
pub(super) fn segment_files(storage: &dyn Storage) -> Result<Vec<String>> {
    Ok(storage
        .list_dir(SEGMENTS_DIR)?
        .into_iter()
        .filter(|name| {
            name.ends_with(".vsec") || name.ends_with(".msec") || name.ends_with(".hidx")
        })
        .collect())
}

/// 删除 MANIFEST 未引用的段文件(崩溃残留的 `Building`/`Obsolete` 孤儿)。
pub(super) fn remove_unreferenced_segments(
    storage: &dyn Storage,
    manifest: &Manifest,
) -> Result<()> {
    let referenced: HashSet<String> = manifest
        .segments
        .iter()
        .flat_map(|segment| {
            [
                vsec_name(segment.segment_id),
                msec_name(segment.segment_id),
                hidx_name(segment.segment_id),
            ]
        })
        .collect();
    for name in segment_files(storage)? {
        if !referenced.contains(&name) {
            storage.remove_if_exists(&format!("{SEGMENTS_DIR}/{name}"))?;
        }
    }
    Ok(())
}

/// 提交 MANIFEST:写 `MANIFEST.<v>` → 写 `current` → 保留最近 `MANIFEST_KEEP` 版。
pub(super) fn commit_manifest(
    storage: &dyn Storage,
    manifest: &Manifest,
    hook: Option<&dyn FsyncHook>,
    encryption: Option<&crate::crypto::Encryption>,
) -> Result<()> {
    let encoded = manifest::encode(manifest)?;
    let bytes =
        crate::crypto::encrypt_file(encryption, b"manifest", manifest.manifest_version, &encoded)?;
    let name = manifest_name(manifest.manifest_version);
    if let Some(hook) = hook {
        hook.before(IoAction::Write {
            file: &name,
            offset: 0,
            len: bytes.len(),
        })?;
    }
    storage.write_atomic(&name, &bytes)?;
    if let Some(hook) = hook {
        hook.before(IoAction::Write {
            file: CURRENT_FILE,
            offset: 0,
            len: manifest.manifest_version.to_string().len(),
        })?;
    }
    storage.write_atomic(
        CURRENT_FILE,
        manifest.manifest_version.to_string().as_bytes(),
    )?;
    // reason: 提交点已过(新的 `MANIFEST.<v>` 已写、`current` 已原子切换);裁剪旧
    // 版本失败只遗留历史文件,不影响已提交状态,绝不因此让调用方以为提交失败
    // (避免"磁盘已换、内存未换"半同步)。
    let _ = prune_manifests(storage).ok();
    Ok(())
}

/// 删除多余的历史 MANIFEST,只保留最近 `MANIFEST_KEEP` 个。
fn prune_manifests(storage: &dyn Storage) -> Result<()> {
    let mut versions: Vec<(u64, String)> = storage
        .list_dir("")?
        .into_iter()
        .filter_map(|name| parse_manifest_name(&name).map(|version| (version, name)))
        .collect();
    versions.sort_by_key(|(version, _)| *version);
    while versions.len() > MANIFEST_KEEP {
        let (_, name) = versions.remove(0);
        storage.remove_if_exists(&name)?;
    }
    Ok(())
}

/// 打开 MANIFEST 所列各段的惰性句柄(设计 04 §8/§11、FC-PERSIST-INV-021)。
///
/// vsec/msec 必须存在且非空;缺失/为空返回 [`MnemeError::Corrupted`](否则会无声丢数据)。
/// hidx 属**可选加速器**:缺失或 CRC 不符时,`fail_fast` 下报 `Corrupted`,否则返回
/// `None`(调用方降级暴力,`check()` 另行报告),与设计 05 §12「索引是优化」一致。
/// 打开只读头部与 `node_table` 所需的文件信息,向量/邻接字节按需缺页。
pub(super) fn open_segment_handles(
    storage: &Arc<dyn Storage>,
    manifest: &Manifest,
    fail_fast: bool,
    encryption: Option<&crate::crypto::Encryption>,
) -> Result<Vec<SegmentBytes>> {
    let mut segments = Vec::new();
    for segment in &manifest.segments {
        let handle = SegmentHandle::open(&SegmentHandleOpenInput {
            storage,
            segment_id: segment.segment_id,
            expected_hidx_crc: segment.hidx_crc,
            fail_fast,
            encryption,
        })?;
        segments.push(SegmentBytes::from_handle(&handle));
    }
    Ok(segments)
}
