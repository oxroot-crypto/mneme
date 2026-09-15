use crate::core::error::{MnemeError, Result};
use crate::persist::{Cursor, FORMAT_VERSION, check_version, crc32};

use super::encode::{HEADER_LEN, MAGIC};

/// 单节点单层邻接表硬上限(防篡改导致巨量分配);与配置校验共用。
const MAX_DEGREE: usize = crate::memory::index::MAX_INDEX_DEGREE as usize;

/// hidx 定长头部字段(解码中间态)。
pub(super) struct HidxHeader {
    pub(super) m: u16,
    pub(super) m0: u16,
    pub(super) ef_construction: u16,
    pub(super) entry_level: u8,
    pub(super) entry_slot: u32,
    pub(super) count: usize,
    pub(super) ml: f32,
    pub(super) node_table_len: usize,
    pub(super) adj_len: usize,
}

/// 校验魔数/版本/头部 CRC 并读取定长头部。
pub(super) fn parse_header(bytes: &[u8]) -> Result<HidxHeader> {
    if bytes.len() < HEADER_LEN as usize {
        return Err(corrupt("文件短于头部"));
    }
    if bytes[0..4] != MAGIC {
        return Err(corrupt("魔数不符"));
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    check_version("hidx", version, FORMAT_VERSION)?;
    let header_len = u16::from_le_bytes([bytes[6], bytes[7]]);
    if header_len != HEADER_LEN {
        return Err(corrupt("header_len 不符"));
    }
    let stored_crc = u32::from_le_bytes([bytes[36], bytes[37], bytes[38], bytes[39]]);
    if crc32(&bytes[0..36]) != stored_crc {
        return Err(corrupt("header_crc32 不符"));
    }

    let mut cursor = Cursor::new(bytes, "hidx 头部");
    let _magic = cursor.take(4)?;
    let _version = cursor.u16()?;
    let _header_len = cursor.u16()?;
    let m = cursor.u16()?;
    let m0 = cursor.u16()?;
    let ef_construction = cursor.u16()?;
    let entry_level = cursor.u8()?;
    let _reserved = cursor.u8()?;
    let ml = f32::from_le_bytes([cursor.u8()?, cursor.u8()?, cursor.u8()?, cursor.u8()?]);
    let count = cursor.u32()? as usize;
    let entry_slot = cursor.u32()?;
    let node_table_len = cursor.u32()? as usize;
    let adj_len = cursor.u32()? as usize;
    validate_header_params(m, m0, ef_construction, ml)?;
    Ok(HidxHeader {
        m,
        m0,
        ef_construction,
        entry_level,
        entry_slot,
        count,
        ml,
        node_table_len,
        adj_len,
    })
}

/// 校验头部图参数落在构建期允许域内(与 `Builder::validate` 同口径);
/// 否则自产文件与手改文件口径不一致。
fn validate_header_params(m: u16, m0: u16, ef_construction: u16, ml: f32) -> Result<()> {
    if m < 2 || m0 < m || m as usize > MAX_DEGREE || m0 as usize > MAX_DEGREE {
        return Err(corrupt("头部度数参数越界"));
    }
    if ef_construction < 1 {
        return Err(corrupt("ef_construction 必须 ≥ 1"));
    }
    if !ml.is_finite() || ml <= 0.0 {
        return Err(corrupt("ml 非正有限值"));
    }
    Ok(())
}

/// 构造 hidx 文件级损坏错误。
pub(super) fn corrupt(reason: &str) -> MnemeError {
    MnemeError::Corrupted {
        segment: None,
        reason: format!("hidx: {reason}"),
    }
}
