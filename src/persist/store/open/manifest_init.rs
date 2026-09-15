//! MANIFEST 载入/初始化同维度、度量身份校验。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::persist::manifest::Manifest;
use crate::persist::store::manifest_io;
use crate::persist::store::wal_writer;
use crate::persist::wal;

use super::options::OpenOptions;

/// 新建库时首个可分配的关系类型编号(内建关系占用 `0..16`)。
const NEXT_REL_KIND_INITIAL: u16 = 16;

/// 载入既有 MANIFEST,或据请求与 WAL 头初始化一个新 MANIFEST。
pub(super) fn load_or_init_manifest(
    storage: &Arc<dyn crate::persist::storage::Storage>,
    options: &OpenOptions,
) -> Result<(Manifest, u64)> {
    let (requested_dimension, requested_metric) = (options.dimension, options.metric);
    let loaded = manifest_io::load_manifest(storage.as_ref(), options.encryption.as_ref())?;
    match loaded {
        Some((manifest, version)) => {
            verify_requested_identity(&manifest, requested_dimension, requested_metric)?;
            Ok((manifest, version))
        }
        None => {
            // 无任何合法 MANIFEST:
            // - 段文件存在且 WAL 含可应用帧 → 「首次 flush 中途崩溃」,段是未提交孤儿,
            //   以 WAL 为准重建、随后清理孤儿段,绝不因此拒绝打开而丢数据;
            // - 段文件存在但 WAL 无可应用帧(无 WAL / 空 WAL / 仅头)→ 来源不明
            //   (可能是 MANIFEST 丢失),拒绝当作新库覆盖(设计 16 §3)。
            let has_segments = !manifest_io::segment_files(storage.as_ref())?.is_empty();
            if has_segments && !wal_has_frames(storage)? {
                return Err(MnemeError::Corrupted {
                    segment: None,
                    reason: "段文件存在但无 MANIFEST 且 WAL 无可应用帧,拒绝覆盖".to_string(),
                });
            }
            Ok((
                init_manifest_from_wal(
                    storage,
                    requested_dimension,
                    requested_metric,
                    options.tuning.stopwords,
                )?,
                0,
            ))
        }
    }
}

/// WAL 文件集是否含至少一个完整可应用帧(区分「首次 flush 崩溃」与「MANIFEST 丢失」)。
fn wal_has_frames(storage: &Arc<dyn crate::persist::storage::Storage>) -> Result<bool> {
    for rel in wal_writer::wal_files(storage.as_ref())? {
        let bytes = storage.read_file(&rel)?;
        match wal::visit_frames(&bytes, |_, _, _, _| Ok(())) {
            Ok(valid_len) if valid_len > wal::FILE_HEADER_LEN => return Ok(true),
            // 头部损坏视作无可应用帧(来源不明,交由上层拒绝覆盖)。
            _ => {}
        }
    }
    Ok(false)
}

/// 校验调用方请求的维度/度量与既有 MANIFEST 一致。
fn verify_requested_identity(
    manifest: &Manifest,
    requested_dimension: Option<u32>,
    requested_metric: Option<Metric>,
) -> Result<()> {
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
    Ok(())
}

/// 无 MANIFEST 但可能有 WAL(崩溃在首次 flush 前):以最早 WAL 文件头为准初始化。
fn init_manifest_from_wal(
    storage: &Arc<dyn crate::persist::storage::Storage>,
    requested_dimension: Option<u32>,
    requested_metric: Option<Metric>,
    stopwords: bool,
) -> Result<Manifest> {
    let wal_header = wal_writer::wal_files(storage.as_ref())?
        .into_iter()
        .find_map(|rel| {
            storage
                .read_file(&rel)
                .ok()
                .and_then(|bytes| wal::parse_file_header(&bytes).ok())
        });
    let dimension = resolve_dimension(requested_dimension, wal_header.as_ref())?;
    let metric = resolve_metric(requested_metric, wal_header.as_ref())?;
    Ok(Manifest {
        dimension,
        metric,
        stopwords,
        next_rel_kind: NEXT_REL_KIND_INITIAL,
        manifest_version: 0,
        watermark_seqno: 0,
        next_rowid: 0,
        next_segment_id: 0,
        next_ns_id: 1,
        namespaces: Vec::new(),
        rel_kinds: Vec::new(),
        segments: Vec::new(),
    })
}

/// 解析最终维度:WAL 头与请求冲突时拒绝,缺失时要求请求显式指定。
fn resolve_dimension(requested: Option<u32>, wal_header: Option<&wal::WalHeader>) -> Result<u32> {
    match (requested, wal_header) {
        (_, None) => requested.ok_or(MnemeError::Config {
            reason: "新建持久库必须指定维度",
        }),
        (Some(d), Some(header)) if header.dimension != d => Err(MnemeError::DimensionMismatch {
            expected: header.dimension,
            got: d as usize,
        }),
        (Some(d), Some(_)) => Ok(d),
        (None, Some(header)) => Ok(header.dimension),
    }
}

/// 解析最终度量:WAL 头与请求冲突时拒绝,缺失时缺省余弦。
fn resolve_metric(
    requested: Option<Metric>,
    wal_header: Option<&wal::WalHeader>,
) -> Result<Metric> {
    match (requested, wal_header) {
        (_, None) => Ok(requested.unwrap_or(Metric::Cosine)),
        (Some(m), Some(header)) if header.metric != m => Err(MnemeError::MetricMismatch {
            existing: header.metric,
            requested: m,
        }),
        (Some(m), Some(_)) => Ok(m),
        (None, Some(header)) => Ok(header.metric),
    }
}
