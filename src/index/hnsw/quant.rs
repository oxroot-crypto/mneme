//! 量化副本校验与建图期临时 i8 码流。

use crate::core::error::Result;
use crate::core::options::{BuildPrecision, VectorFormat};
use crate::memory::index::{IndexNode, QuantCopy};

use super::model::BuildCodes;

/// 校验量化副本行数/单行长度/维度与图节点一致。
///
/// # Errors
/// 副本格式为 `F32`、行数或单行长度不符、i8 参数表长度与维度不符,或 f16
/// 副本自推维度与索引节点维度不符时返回
/// [`MnemeError::Corrupted`](crate::core::error::MnemeError::Corrupted)。
pub(super) fn validate_quant(
    quant: &Option<QuantCopy>,
    count: usize,
    dimension: usize,
) -> Result<()> {
    let Some(copy) = quant else {
        return Ok(());
    };
    let corrupt = |reason: &str| crate::core::error::MnemeError::Corrupted {
        segment: None,
        reason: format!("hnsw: {reason}"),
    };
    if copy.format == VectorFormat::F32 || copy.rows.len() != count {
        return Err(corrupt("量化副本格式/行数与索引节点不符"));
    }
    let stride = copy.code_stride();
    if copy.rows.stride() != stride {
        return Err(corrupt("量化副本单行长度与维度不符"));
    }
    match copy.format {
        VectorFormat::I8Rescored if copy.params.len() != dimension * 2 => {
            Err(corrupt("i8 参数表长度与维度不符"))
        }
        VectorFormat::F16 if copy.dimension() != dimension => {
            Err(corrupt("f16 副本维度与索引节点不符"))
        }
        _ => Ok(()),
    }
}

/// 按建图精度档位生成段内临时 i8 码流(`F32` 档或空段返回 `None`)。
///
/// 参数/编码复用 `FC-QUANT-POST-001` 的段级逐维口径;码流只供构建期遍历距离
/// 计算,不写入任何文件(FC-INDEX-POST-010)。
///
/// # Errors
/// 段内向量维度不一致或含非有限分量时返回结构化错误,绝不静默跳过量化。
pub(super) fn build_codes(
    precision: BuildPrecision,
    nodes: &[IndexNode],
    count: usize,
    dimension: usize,
) -> Result<Option<BuildCodes>> {
    if precision == BuildPrecision::F32 || count == 0 || dimension == 0 {
        return Ok(None);
    }
    let vectors: Vec<&[f32]> = nodes[..count].iter().map(|node| &node.vector[..]).collect();
    let params = crate::quant::scalar_i8::build_params(&vectors, dimension)?;
    let mut codes = Vec::with_capacity(count.saturating_mul(dimension));
    for vector in &vectors {
        crate::quant::scalar_i8::encode_row_into(&mut codes, vector, &params);
    }
    Ok(Some(BuildCodes { params, codes }))
}

/// 解析并缓存 i8 副本的段级参数(仅 `I8Rescored` 副本;畸形参数退回 `None`)。
///
/// 参数与图节点同寿命且不可变,查询期只需按查询向量预计算权重;解析失败与
/// [`quantize_query`](super::HnswIndex::quantize_query) 的旧口径一致:本次查询退 f32 精确路径。
pub(super) fn cached_i8_params(
    quant: &Option<QuantCopy>,
    dimension: usize,
) -> Option<crate::quant::scalar_i8::I8Params> {
    let copy = quant.as_ref()?;
    if copy.format != VectorFormat::I8Rescored {
        return None;
    }
    crate::quant::scalar_i8::I8Params::from_table(&copy.params, dimension).ok()
}
