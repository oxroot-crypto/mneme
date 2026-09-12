//! MANIFEST 载入 / 提交 / 裁剪与段文件清理(`store/manifest_io.rs`)。
//!
//! 这些是 [`Store`](super::Store) 打开与 flush 的纯文件系统辅助:不含
//! 业务状态,只负责目录扫描、原子提交与保留最近 `MANIFEST_KEEP` 版。

use std::collections::HashSet;
use std::path::Path;

use crate::core::error::{MnemeError, Result};
use crate::persist::crc32;
use crate::persist::hook::{FsyncHook, IoAction};
use crate::persist::manifest::{self, Manifest};
use crate::persist::recover::SegmentBytes;
use crate::persist::storage::{
    self, CURRENT_FILE, MANIFEST_KEEP, SEGMENTS_DIR, WAL_DIR, hidx_name, manifest_name, msec_name,
    parse_manifest_name, vsec_name,
};

/// 清理 `segments/`、`wal/` 与根目录下的 `.tmp` 半成品(崩溃点 `Building` 孤儿)。
pub(super) fn cleanup_orphans(root: &Path) -> Result<()> {
    for dir in [SEGMENTS_DIR, WAL_DIR, ""] {
        for name in storage::list_dir(root, dir)? {
            if !name.ends_with(".tmp") {
                continue;
            }
            let rel = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            storage::remove_if_exists(&storage::resolve(root, &rel)?)?;
        }
    }
    Ok(())
}

/// 读取 `current` 或扫描目录得到最新合法 MANIFEST。
///
/// 返回 `Ok(None)` 仅表示"全新库"(`current`/`MANIFEST.*` 均不存在)。
/// 若 `current` 存在但 `current` 指向的 MANIFEST 与目录内其余 `MANIFEST.*`
/// 全部非法,返回 [`MnemeError::Corrupted`]——**绝不**当作新库覆盖(设计 16 §3)。
pub(super) fn load_manifest(root: &Path) -> Result<Option<(Manifest, u64)>> {
    let has_current = storage::exists(&root.join(CURRENT_FILE))?;
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
    if best.is_none() && has_current {
        return Err(MnemeError::Corrupted {
            segment: None,
            reason: "current 存在但无任何 CRC 合法的 MANIFEST".to_string(),
        });
    }
    Ok(best)
}

/// 列出 `segments/` 下的段文件(`.vsec`/`.msec`/`.hidx`)。
pub(super) fn segment_files(root: &Path) -> Result<Vec<String>> {
    Ok(storage::list_dir(root, SEGMENTS_DIR)?
        .into_iter()
        .filter(|name| {
            name.ends_with(".vsec") || name.ends_with(".msec") || name.ends_with(".hidx")
        })
        .collect())
}

/// 删除 MANIFEST 未引用的段文件(崩溃残留的 `Building`/`Obsolete` 孤儿)。
pub(super) fn remove_unreferenced_segments(root: &Path, manifest: &Manifest) -> Result<()> {
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
    for name in segment_files(root)? {
        if !referenced.contains(&name) {
            storage::remove_if_exists(&storage::resolve(root, &format!("{SEGMENTS_DIR}/{name}"))?)?;
        }
    }
    Ok(())
}

/// 提交 MANIFEST:写 `MANIFEST.<v>` → 写 `current` → 保留最近 `MANIFEST_KEEP` 版。
pub(super) fn commit_manifest(
    root: &Path,
    manifest: &Manifest,
    hook: Option<&dyn FsyncHook>,
) -> Result<()> {
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
    // reason: 提交点已过(新的 `MANIFEST.<v>` 已写、`current` 已原子切换);裁剪旧
    // 版本失败只遗留历史文件,不影响已提交状态,绝不因此让调用方以为提交失败
    // (避免"磁盘已换、内存未换"半同步)。
    let _ = prune_manifests(root);
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
///
/// vsec/msec 必须存在且非空;缺失/为空返回 [`MnemeError::Corrupted`](否则会无声丢数据)。
/// hidx 属**可选加速器**:缺失或 CRC 不符时,`fail_fast` 下报 `Corrupted`,否则返回
/// `None`(调用方降级暴力,`check()` 另行报告),与设计 05 §12「索引是优化」一致。
pub(super) fn read_segment_bytes(
    root: &Path,
    manifest: &Manifest,
    fail_fast: bool,
) -> Result<Vec<SegmentBytes>> {
    let mut segments = Vec::new();
    for segment in &manifest.segments {
        let id = segment.segment_id;
        let vsec = read_required_segment(root, id, &vsec_name(id))?;
        let msec = read_required_segment(root, id, &msec_name(id))?;
        let hidx = if segment.hidx_crc == 0 {
            None
        } else {
            read_optional_index(root, id, segment.hidx_crc, fail_fast)?
        };
        segments.push(SegmentBytes {
            segment_id: id,
            vsec,
            msec,
            hidx,
        });
    }
    Ok(segments)
}

/// 读取可选的 hidx 文件:存在且整文件 CRC 与 MANIFEST 相符时返回其字节。
///
/// 缺失/为空/CRC 不符时:可写非 fail-fast 打开返回 `None`(降级暴力);fail-fast
/// 返回 [`MnemeError::Corrupted`]。
fn read_optional_index(
    root: &Path,
    segment_id: u32,
    expected_crc: u32,
    fail_fast: bool,
) -> Result<Option<Vec<u8>>> {
    let name = hidx_name(segment_id);
    let rel = format!("{SEGMENTS_DIR}/{name}");
    let path = storage::resolve(root, &rel)?;
    let bytes = match crate::persist::source::read_whole(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return if fail_fast {
                Err(MnemeError::Corrupted {
                    segment: Some(crate::core::types::SegmentId::new(segment_id)),
                    reason: format!("MANIFEST 引用的 hidx 缺失:{name}"),
                })
            } else {
                Ok(None)
            };
        }
        Err(error) => return Err(error.into()),
    };
    if bytes.is_empty() || crc32(&bytes) != expected_crc {
        return if fail_fast {
            Err(MnemeError::Corrupted {
                segment: Some(crate::core::types::SegmentId::new(segment_id)),
                reason: format!("hidx 文件 CRC 与 MANIFEST 不符:{name}"),
            })
        } else {
            Ok(None)
        };
    }
    Ok(Some(bytes))
}

/// 读取一个被 MANIFEST 引用的段文件;不存在或为空返回 [`MnemeError::Corrupted`]。
fn read_required_segment(root: &Path, segment_id: u32, name: &str) -> Result<Vec<u8>> {
    let rel = format!("{SEGMENTS_DIR}/{name}");
    let path = storage::resolve(root, &rel)?;
    let bytes = match crate::persist::source::read_whole(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(MnemeError::Corrupted {
                segment: Some(crate::core::types::SegmentId::new(segment_id)),
                reason: format!("MANIFEST 引用的段文件缺失:{name}"),
            });
        }
        Err(error) => return Err(error.into()),
    };
    if bytes.is_empty() {
        return Err(MnemeError::Corrupted {
            segment: Some(crate::core::types::SegmentId::new(segment_id)),
            reason: format!("MANIFEST 引用的段文件为空:{name}"),
        });
    }
    Ok(bytes)
}
