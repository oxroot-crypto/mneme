//! 文件级加解密门面与 feature 门控(设计 11 §2)。

use crate::core::error::{MnemeError, Result};

use super::envelope::{is_envelope, open, seal};
use super::key::Encryption;

/// feature 门控:未开 `encrypt` 时所有加密操作显式 `Unsupported`。
#[cfg(not(feature = "encrypt"))]
pub(crate) fn ensure_supported(encryption: Option<&Encryption>) -> Result<()> {
    if encryption.is_some() {
        return Err(MnemeError::Unsupported { feature: "encrypt" });
    }
    Ok(())
}

/// feature 门控:开启时无额外限制。
#[cfg(feature = "encrypt")]
pub(crate) fn ensure_supported(_encryption: Option<&Encryption>) -> Result<()> {
    Ok(())
}

/// 写盘前处理:配置了加密则封装为信封,否则**原样借用**返回(零拷贝)。
///
/// # Errors
/// feature 未开启却配置了加密 → [`MnemeError::Unsupported`];加密失败透传。
pub(crate) fn encrypt_file<'a>(
    encryption: Option<&Encryption>,
    scope: &[u8],
    identity: u64,
    bytes: &'a [u8],
) -> Result<std::borrow::Cow<'a, [u8]>> {
    match encryption {
        Some(encryption) => seal(encryption, scope, identity, bytes).map(std::borrow::Cow::Owned),
        None => Ok(std::borrow::Cow::Borrowed(bytes)),
    }
}

/// 读盘后处理:信封则解密(未开 feature/无密钥 → 结构化错误),明文原样返回。
///
/// # Errors
/// 信封但未配置加密(未开 feature 或库未启用)→ `Unsupported`;解密失败 → `Corrupted`。
pub(crate) fn decrypt_file(
    encryption: Option<&Encryption>,
    scope: &[u8],
    identity: u64,
    bytes: Vec<u8>,
) -> Result<Vec<u8>> {
    if !is_envelope(&bytes) {
        return Ok(bytes);
    }
    let Some(encryption) = encryption else {
        return Err(MnemeError::Unsupported { feature: "encrypt" });
    };
    open(encryption, scope, identity, &bytes)
}
