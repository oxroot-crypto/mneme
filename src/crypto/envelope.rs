//! AES-256-GCM 信封编解码与 feature 门控占位(设计 11 §2)。

#[cfg(feature = "encrypt")]
use aes_gcm::aead::{Aead, KeyInit};
#[cfg(feature = "encrypt")]
use aes_gcm::{Aes256Gcm, Nonce};

use crate::core::error::{MnemeError, Result};

use super::key::Encryption;
#[cfg(feature = "encrypt")]
use super::key::KeyId;

/// 加密信封魔数(加密文件首 4 字节;明文段不以它开头)。
pub(crate) const ENVELOPE_MAGIC: [u8; 4] = *b"MNEC";
/// 信封固定头长度(魔数 4 + 版本 2 + key_id 4 + 明文长度 4 + nonce 12)。
const ENVELOPE_HEADER: usize = 26;
/// AES-GCM 认证标签长度。
const TAG_LEN: usize = 16;

/// 是否为加密信封(按魔数判定,不接触密钥)。
pub(crate) fn is_envelope(bytes: &[u8]) -> bool {
    bytes.len() >= ENVELOPE_HEADER + TAG_LEN && bytes[0..4] == ENVELOPE_MAGIC
}

/// 信封头部的 `key_id`;非信封(或头不足 10 字节)返回 `None`
/// (仅供迁移统计,不接触密钥)。
pub(crate) fn envelope_key_id(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 10 || bytes[0..4] != ENVELOPE_MAGIC {
        return None;
    }
    Some(u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]))
}

/// 信封 AAD:用途标签 + 标识 + 格式版本 + 头字段(绑定位置与长度)。
#[cfg(feature = "encrypt")]
fn aad(scope: &[u8], identity: u64, header: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(scope.len() + 12 + header.len());
    out.extend_from_slice(scope);
    out.extend_from_slice(&identity.to_le_bytes());
    out.extend_from_slice(header);
    out
}

/// 用当前 active 密钥加密 `plaintext` 为信封。
///
/// # Arguments
/// * `encryption` - 密钥提供者与算法。
/// * `scope` - 用途标签(如 `b"vsec"`/`b"wal"`),参与 AAD 防跨用途搬运。
/// * `identity` - 文件/段/版本标识(参与 AAD)。
/// * `plaintext` - 明文。
///
/// # Errors
/// active 密钥不可用或 AEAD 失败时返回结构化错误(绝不落半写密文)。
#[cfg(feature = "encrypt")]
pub(crate) fn seal(
    encryption: &Encryption,
    scope: &[u8],
    identity: u64,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let key_id = encryption.provider.active_key();
    let key = encryption.provider.key(key_id)?;
    let mut nonce = [0_u8; 12];
    getrandom::fill(&mut nonce).map_err(|_error| MnemeError::Inconsistent {
        reason: "crypto: OS 熵源不可用",
    })?;
    let mut header = Vec::with_capacity(ENVELOPE_HEADER - 4);
    header.extend_from_slice(&crate::persist::FORMAT_VERSION.to_le_bytes());
    header.extend_from_slice(&key_id.0.to_le_bytes());
    header.extend_from_slice(
        &u32::try_from(plaintext.len())
            .map_err(|_| MnemeError::TooLarge {
                field: "crypto plaintext",
                limit: u32::MAX as usize,
                got: plaintext.len(),
            })?
            .to_le_bytes(),
    );
    header.extend_from_slice(&nonce);
    let cipher = Aes256Gcm::new_from_slice(&key.0).map_err(|_error| MnemeError::Inconsistent {
        reason: "crypto: 密钥长度非法",
    })?;
    let nonce_array = Nonce::try_from(&nonce[..]).map_err(|_error| MnemeError::Inconsistent {
        reason: "crypto: nonce 长度非法",
    })?;
    let ciphertext = cipher
        .encrypt(
            &nonce_array,
            aes_gcm::aead::Payload {
                msg: plaintext,
                aad: &aad(scope, identity, &header),
            },
        )
        .map_err(|_error| MnemeError::Inconsistent {
            reason: "crypto: AEAD 加密失败",
        })?;
    let mut out = Vec::with_capacity(ENVELOPE_HEADER + ciphertext.len());
    out.extend_from_slice(&ENVELOPE_MAGIC);
    out.extend_from_slice(&header);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// 解密信封;错误密钥/被篡改/版本不符一律 `Corrupted`。
///
/// # Errors
/// 非信封、长度越界、密钥不可用或认证失败时返回 [`MnemeError::Corrupted`]。
#[cfg(feature = "encrypt")]
pub(crate) fn open(
    encryption: &Encryption,
    scope: &[u8],
    identity: u64,
    envelope: &[u8],
) -> Result<Vec<u8>> {
    let corrupt = |reason: &str| MnemeError::Corrupted {
        segment: None,
        reason: format!("crypto: {reason}"),
    };
    if !is_envelope(envelope) {
        return Err(corrupt("非加密信封或长度不足"));
    }
    let header = &envelope[4..ENVELOPE_HEADER];
    let version = u16::from_le_bytes([header[0], header[1]]);
    crate::persist::check_version("crypto envelope", version, crate::persist::FORMAT_VERSION)?;
    let key_id = u32::from_le_bytes([header[2], header[3], header[4], header[5]]);
    let plaintext_len = u32::from_le_bytes([header[6], header[7], header[8], header[9]]) as usize;
    let nonce = &header[10..22];
    let key = encryption.provider.key(KeyId(key_id))?;
    let cipher = Aes256Gcm::new_from_slice(&key.0).map_err(|_error| MnemeError::Inconsistent {
        reason: "crypto: 密钥长度非法",
    })?;
    let nonce_array = Nonce::try_from(nonce).map_err(|_error| MnemeError::Inconsistent {
        reason: "crypto: nonce 长度非法",
    })?;
    let plaintext = cipher
        .decrypt(
            &nonce_array,
            aes_gcm::aead::Payload {
                msg: &envelope[ENVELOPE_HEADER..],
                aad: &aad(scope, identity, header),
            },
        )
        .map_err(|_error| corrupt("认证失败(错误密钥或数据被篡改)"))?;
    if plaintext.len() != plaintext_len {
        return Err(corrupt("明文长度与头声明不符"));
    }
    Ok(plaintext)
}

/// 未开 `encrypt` feature 的占位:任何加密写入都显式拒绝。
#[cfg(not(feature = "encrypt"))]
pub(crate) fn seal(
    _encryption: &Encryption,
    _scope: &[u8],
    _identity: u64,
    _plaintext: &[u8],
) -> Result<Vec<u8>> {
    Err(MnemeError::Unsupported { feature: "encrypt" })
}

/// 未开 `encrypt` feature 的占位:信封一律 `Unsupported`(绝不按明文解析)。
#[cfg(not(feature = "encrypt"))]
pub(crate) fn open(
    _encryption: &Encryption,
    _scope: &[u8],
    _identity: u64,
    _envelope: &[u8],
) -> Result<Vec<u8>> {
    Err(MnemeError::Unsupported { feature: "encrypt" })
}
