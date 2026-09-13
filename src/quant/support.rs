//! 量化格式的 feature 门控(`quant/support.rs`)。
//!
//! 单点定义「当前构建支持哪些量化格式」,供构造期(L1 Builder)、打开期(L2)
//! 与恢复期(L2)共用,绝不静默降级。

use crate::core::error::{MnemeError, Result};
use crate::core::options::VectorFormat;

/// 校验当前构建是否支持该量化格式:未开 `quant-f16` 时 `F16` 返回
/// `Unsupported { feature: "quant-f16" }`,绝不静默降级(FC-QUANT-ERR-001)。
///
/// # Errors
/// `format == F16` 且未启用 `quant-f16` feature 时返回 [`MnemeError::Unsupported`]。
pub(crate) fn ensure_format_supported(format: VectorFormat) -> Result<()> {
    if format == VectorFormat::F16 && !cfg!(feature = "quant-f16") {
        return Err(MnemeError::Unsupported {
            feature: "quant-f16",
        });
    }
    Ok(())
}
