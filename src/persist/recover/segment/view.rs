//! 段视图解析同 payload 校验。

use crate::core::error::{MnemeError, Result};
use crate::persist::msec;
use crate::persist::vsec;

use super::super::SegmentBytes;

/// 解析单个段的 vsec/msec 视图并校验 payload;损坏且非 fail-fast 时返回 `None`。
pub(in crate::persist::recover) fn load_segment_views<'a>(
    segment: &'a SegmentBytes,
    verify_payload: bool,
    fail_fast: bool,
) -> Result<Option<(vsec::VsecView<'a>, msec::MsecView<'a>)>> {
    let mut vsec_view = match vsec::parse(segment.vsec_bytes()?) {
        Ok(view) => view,
        // 版本不一致一律拒绝打开(I18),绝不因 fail-fast 关闭而降级为跳过。
        Err(error) if fail_fast || is_version_rejection(&error) => return Err(error),
        Err(_) => return Ok(None),
    };
    // f16 段在未开 `quant-f16` 的构建上拒绝打开,绝不静默按 f32 服务
    // (FC-QUANT-ERR-002);该判断先于 fail-fast 降级,数据仍完整可读也须显式报错。
    crate::quant::ensure_format_supported(vsec_view.quant())?;
    let mut msec_view = match msec::parse(segment.msec_bytes()?) {
        Ok(view) => view,
        Err(error) if fail_fast || is_version_rejection(&error) => return Err(error),
        Err(_) => return Ok(None),
    };
    if verify_payload {
        if let Err(error) = vsec_view.verify_payload() {
            if fail_fast {
                return Err(error);
            }
            return Ok(None);
        }
        if let Err(error) = msec_view.verify_payload() {
            if fail_fast {
                return Err(error);
            }
            return Ok(None);
        }
    }
    Ok(Some((vsec_view, msec_view)))
}

/// 是否为格式版本不匹配导致的拒绝(不可降级跳过,I18)。
fn is_version_rejection(error: &MnemeError) -> bool {
    matches!(error, MnemeError::UnsupportedVersion { .. })
}
