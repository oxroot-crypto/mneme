//! L6 量化原语(i8 标量量化、f16、两阶段候选预算)。
//!
//! 本模块是**纯原语模块**:无 I/O、无锁、无全局状态,依赖等级同 L0
//! (见 `docs/design/08-l6-quant.md` 落地状态),供 L2 段编码与 L3 索引打分直接复用。
//! 高层编排(建段抽样回退、`stats()` 聚合、async 门面)留在各自所属层。

use crate::core::error::{MnemeError, Result};
use crate::core::options::VectorFormat;

pub(crate) mod rescore;
pub(crate) mod scalar_i8;

#[cfg(feature = "quant-f16")]
pub(crate) mod f16;

/// 校验当前构建是否支持该量化格式:未开 `quant-f16` 时 `F16` 返回
/// `Unsupported { feature: "quant-f16" }`,绝不静默降级(FC-QUANT-ERR-001)。
pub(crate) fn ensure_format_supported(format: VectorFormat) -> Result<()> {
    if format == VectorFormat::F16 && !cfg!(feature = "quant-f16") {
        return Err(MnemeError::Unsupported {
            feature: "quant-f16",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FC-QUANT-ERR-001:feature 门控只拦 `F16`,`F32`/`I8Rescored` 恒可用。
    #[test]
    fn format_support_matches_feature_gate() {
        assert!(ensure_format_supported(VectorFormat::F32).is_ok());
        assert!(ensure_format_supported(VectorFormat::I8Rescored).is_ok());
        #[cfg(feature = "quant-f16")]
        assert!(ensure_format_supported(VectorFormat::F16).is_ok());
        #[cfg(not(feature = "quant-f16"))]
        assert!(matches!(
            ensure_format_supported(VectorFormat::F16),
            Err(MnemeError::Unsupported {
                feature: "quant-f16"
            })
        ));
    }
}
