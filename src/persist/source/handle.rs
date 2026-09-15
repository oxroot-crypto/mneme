//! 段句柄集:`SegmentHandle` 的打开与校验(vsec/msec 存在性、hidx CRC)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::types::SegmentId;
use crate::memory::lazy::ByteSource;
use crate::persist::storage::{Storage, hidx_name, msec_name, vsec_name};

use super::byte_file::{ByteFile, ByteFileOpenInput};

/// 一个段的三文件句柄集(vsec/msec/可选 hidx)。
///
/// 持 [`Arc<ByteFile>`] 即保证对应文件在句柄存活期内可读(惰性向量与惰性图
/// 各自持有 vsec/hidx 句柄);compaction 移动旧段只影响目录项,不影响已打开的
/// 描述符/映射(FC-PERSIST-INV-021)。
#[derive(Debug)]
pub(crate) struct SegmentHandle {
    /// 段编号。
    pub(crate) segment_id: u32,
    /// 向量段文件。
    pub(crate) vsec: Arc<ByteFile>,
    /// 元数据段文件。
    pub(crate) msec: Arc<ByteFile>,
    /// HNSW 图文件(MANIFEST `hidx_crc == 0` 时为 `None`)。
    pub(crate) hidx: Option<Arc<ByteFile>>,
}

/// [`SegmentHandle::open`] 的输入参数。
pub(crate) struct SegmentHandleOpenInput<'a> {
    /// 存储后端。
    pub(crate) storage: &'a Arc<dyn Storage>,
    /// 段编号。
    pub(crate) segment_id: u32,
    /// MANIFEST 记录的 hidx 整文件 CRC(`0` = 无索引)。
    pub(crate) expected_hidx_crc: u32,
    /// hidx 缺失/损坏时是否直接上报(`false` = 降级暴力)。
    pub(crate) fail_fast: bool,
    /// 静态加密配置(`None` = 明文)。
    pub(crate) encryption: Option<&'a crate::crypto::Encryption>,
}

impl SegmentHandle {
    /// 打开一个段的三文件句柄,校验存在性/非空与 hidx 整文件 CRC。
    ///
    /// vsec/msec 缺失或为空 → [`MnemeError::Corrupted`](I2/I3);hidx 缺失/为空/
    /// CRC 不符时:`fail_fast` 报 `Corrupted`,否则返回 `None`(降级暴力,与
    /// 设计 05 §12「索引是优化」一致);`expected_hidx_crc == 0` 表示无索引。
    ///
    /// # Errors
    /// 见上;其余 I/O 失败返回 [`MnemeError::Io`]。
    pub(crate) fn open(input: &SegmentHandleOpenInput<'_>) -> Result<Self> {
        let vsec = open_required(&OpenRequiredInput {
            storage: input.storage,
            segment_id: input.segment_id,
            scope: b"vsec",
            name: &vsec_name(input.segment_id),
            encryption: input.encryption,
        })?;
        let msec = open_required(&OpenRequiredInput {
            storage: input.storage,
            segment_id: input.segment_id,
            scope: b"msec",
            name: &msec_name(input.segment_id),
            encryption: input.encryption,
        })?;
        let hidx = open_optional(&OpenOptionalInput {
            storage: input.storage,
            segment_id: input.segment_id,
            expected_crc: input.expected_hidx_crc,
            fail_fast: input.fail_fast,
            encryption: input.encryption,
        })?;
        Ok(Self {
            segment_id: input.segment_id,
            vsec,
            msec,
            hidx,
        })
    }
}

/// [`open_required`] 的输入参数。
struct OpenRequiredInput<'a> {
    /// 存储后端。
    storage: &'a Arc<dyn Storage>,
    /// 段编号。
    segment_id: u32,
    /// 加密信封作用域(如 `b"vsec"`)。
    scope: &'a [u8],
    /// 文件相对名。
    name: &'a str,
    /// 静态加密配置(`None` = 明文)。
    encryption: Option<&'a crate::crypto::Encryption>,
}

/// 打开被 MANIFEST 引用的段文件;不存在或为空 → `Corrupted`。
fn open_required(input: &OpenRequiredInput<'_>) -> Result<Arc<ByteFile>> {
    let OpenRequiredInput {
        storage,
        segment_id,
        scope,
        name,
        encryption,
    } = *input;
    match ByteFile::open(&ByteFileOpenInput {
        storage,
        segment_id,
        scope,
        name,
        encryption,
    }) {
        Ok(file) if file.len() > 0 => Ok(file),
        Ok(_) => Err(MnemeError::Corrupted {
            segment: Some(SegmentId::new(segment_id)),
            reason: format!("MANIFEST 引用的段文件为空:{name}"),
        }),
        Err(MnemeError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(MnemeError::Corrupted {
                segment: Some(SegmentId::new(segment_id)),
                reason: format!("MANIFEST 引用的段文件缺失:{name}"),
            })
        }
        Err(error) => Err(error),
    }
}

/// [`open_optional`] 的输入参数。
struct OpenOptionalInput<'a> {
    /// 存储后端。
    storage: &'a Arc<dyn Storage>,
    /// 段编号。
    segment_id: u32,
    /// 期望的 hidx 整文件 CRC(`0` = 无索引)。
    expected_crc: u32,
    /// hidx 缺失/损坏时是否直接上报(`false` = 降级暴力)。
    fail_fast: bool,
    /// 静态加密配置(`None` = 明文)。
    encryption: Option<&'a crate::crypto::Encryption>,
}

/// 打开可选 hidx 文件并核对整文件 CRC;失败按 `fail_fast` 决定上报或降级。
fn open_optional(input: &OpenOptionalInput<'_>) -> Result<Option<Arc<ByteFile>>> {
    let OpenOptionalInput {
        storage,
        segment_id,
        expected_crc,
        fail_fast,
        encryption,
    } = *input;
    if expected_crc == 0 {
        return Ok(None);
    }
    let name = hidx_name(segment_id);
    let file = match ByteFile::open(&ByteFileOpenInput {
        storage,
        segment_id,
        scope: b"hidx",
        name: &name,
        encryption,
    }) {
        Ok(file) => file,
        Err(MnemeError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return if fail_fast {
                Err(MnemeError::Corrupted {
                    segment: Some(SegmentId::new(segment_id)),
                    reason: format!("MANIFEST 引用的 hidx 缺失:{name}"),
                })
            } else {
                Ok(None)
            };
        }
        Err(error) => return Err(error),
    };
    let crc_ok = file
        .slice_at(0, file.len())
        .is_some_and(|bytes| crate::persist::crc32(bytes) == expected_crc);
    if !crc_ok {
        return if fail_fast {
            Err(MnemeError::Corrupted {
                segment: Some(SegmentId::new(segment_id)),
                reason: format!("hidx 文件 CRC 与 MANIFEST 不符:{name}"),
            })
        } else {
            Ok(None)
        };
    }
    Ok(Some(file))
}
