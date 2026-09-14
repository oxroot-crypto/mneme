//! 静态加密(L11,设计 11 §2):AES-256-GCM 整文件信封。
//!
//! 威胁模型只保护**静态介质**:密文文件为自描述信封
//! `[MNEC][u16 格式版本][u32 key_id][u32 明文长度][12B nonce][密文][16B tag]`,
//! AAD 绑定 `(用途, 段号/版本, 格式版本)` 防跨文件搬运;密钥经 [`KeyProvider`]
//! 注入,引擎不管理密钥文件。开启加密的段以整文件解密进入自有缓冲(设计取舍:
//! mmap 零拷贝对加密段失效),解密失败/错误密钥一律 `Corrupted`,绝不返回错误
//! 数据(不变量 I28 的加密版)。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

#[cfg(feature = "encrypt")]
use aes_gcm::aead::{Aead, KeyInit};
#[cfg(feature = "encrypt")]
use aes_gcm::{Aes256Gcm, Nonce};

use crate::core::error::{MnemeError, Result};

/// 加密信封魔数(加密文件首 4 字节;明文段不以它开头)。
pub(crate) const ENVELOPE_MAGIC: [u8; 4] = *b"MNEC";
/// 信封固定头长度(魔数 4 + 版本 2 + key_id 4 + 明文长度 4 + nonce 12)。
const ENVELOPE_HEADER: usize = 26;
/// AES-GCM 认证标签长度。
const TAG_LEN: usize = 16;
/// AES-256 密钥长度。
const KEY_LEN: usize = 32;

/// 密钥标识:写入信封头,解密时按 id 向 provider 取密钥。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyId(pub u32);

/// 32 字节对称密钥(刻意不实现会打印明文内容的 `Debug`)。
#[derive(Clone, PartialEq, Eq)]
pub struct Key([u8; KEY_LEN]);

impl fmt::Debug for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Key(<redacted>)")
    }
}

impl Key {
    /// 由 32 字节原始密钥构造。
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// 生成随机密钥(CSPRNG:OS 熵源;需 feature `encrypt`)。
    ///
    /// # Errors
    /// OS 熵源不可用时返回 [`MnemeError::Inconsistent`];feature 未开启时返回
    /// [`MnemeError::Unsupported`]。
    #[cfg(feature = "encrypt")]
    pub fn generate() -> Result<Self> {
        let mut bytes = [0_u8; KEY_LEN];
        getrandom::fill(&mut bytes).map_err(|_error| MnemeError::Inconsistent {
            reason: "crypto: OS 熵源不可用",
        })?;
        Ok(Self(bytes))
    }
}

/// 支持的 AEAD 算法(当前仅 AES-256-GCM)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cipher {
    /// AES-256-GCM。
    #[default]
    Aes256Gcm,
}

/// 密钥提供者:宿主实现(环境变量 / OS keychain / KMS / 自管)。
///
/// 引擎只按 id 取密钥,不接触密钥文件;`rotate` 生成新密钥并设为 active。
pub trait KeyProvider: Send + Sync {
    /// 当前用于写入的密钥 id(写入文件头,解密时按 id 取密钥)。
    fn active_key(&self) -> KeyId;

    /// 按 id 取密钥。
    ///
    /// # Errors
    /// 未知 id 或密钥不可用时返回 [`MnemeError::Corrupted`],错误信息不泄露细节。
    fn key(&self, id: KeyId) -> Result<Key>;

    /// 生成新密钥并设为 active,返回新 id(可选能力,默认不支持)。
    ///
    /// # Errors
    /// 未实现轮换时返回 [`MnemeError::Unsupported`]。
    fn rotate(&self) -> Result<KeyId> {
        Err(MnemeError::Unsupported {
            feature: "KeyProvider::rotate",
        })
    }
}

/// 加密配置:密钥提供者 + 算法。
#[derive(Clone)]
pub struct Encryption {
    /// 密钥提供者。
    pub provider: Arc<dyn KeyProvider>,
    /// AEAD 算法。
    pub cipher: Cipher,
}

impl fmt::Debug for Encryption {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Encryption")
            .field("provider", &"Arc<dyn KeyProvider>")
            .field("cipher", &self.cipher)
            .finish()
    }
}

/// 内存密钥环(测试与宿主便捷实现):支持多密钥共存与退役。
#[derive(Debug, Default)]
pub struct Keyring {
    inner: Mutex<KeyringInner>,
}

#[derive(Debug, Default)]
struct KeyringInner {
    active: Option<KeyId>,
    keys: HashMap<u32, Key>,
}

impl Keyring {
    /// 以单个密钥建立密钥环(该密钥即 active)。
    pub fn new(id: KeyId, key: Key) -> Self {
        let mut keys = HashMap::new();
        keys.insert(id.0, key);
        Self {
            inner: Mutex::new(KeyringInner {
                active: Some(id),
                keys,
            }),
        }
    }

    /// 登记一个历史密钥(不改变 active)。
    pub fn insert(&self, id: KeyId, key: Key) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.keys.insert(id.0, key);
        inner.active.get_or_insert(id);
    }

    /// 退役一个密钥(轮换迁移完成后调用);active 密钥不可退役。
    ///
    /// # Returns
    /// 该 id 是否存在并被移除。
    pub fn retire(&self, id: KeyId) -> bool {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if inner.active == Some(id) {
            return false;
        }
        inner.keys.remove(&id.0).is_some()
    }
}

impl KeyProvider for Keyring {
    fn active_key(&self) -> KeyId {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active
            .unwrap_or(KeyId(0))
    }

    fn key(&self, id: KeyId) -> Result<Key> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys
            .get(&id.0)
            .cloned()
            .ok_or_else(|| MnemeError::Corrupted {
                segment: None,
                reason: format!("crypto: 密钥 {} 不可用", id.0),
            })
    }

    fn rotate(&self) -> Result<KeyId> {
        #[cfg(not(feature = "encrypt"))]
        {
            return Err(MnemeError::Unsupported { feature: "encrypt" });
        }
        #[cfg(feature = "encrypt")]
        {
            let key = Key::generate()?;
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // 编号单调分配;耗尽时显式失败,绝不 `saturating_add` 后静默复用
            // 同编号覆盖已有密钥(FC-SEC-POST-001)。
            let next = match inner.active {
                Some(id) => id.0.checked_add(1),
                None => Some(0),
            };
            let Some(next) = next else {
                return Err(MnemeError::IdExhausted { kind: "key_id" });
            };
            let id = KeyId(next);
            inner.keys.insert(next, key);
            inner.active = Some(id);
            Ok(id)
        }
    }
}

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

#[cfg(all(test, feature = "encrypt"))]
mod tests {
    use super::*;

    fn encryption() -> Encryption {
        Encryption {
            provider: Arc::new(Keyring::new(KeyId(7), Key::from_bytes([9_u8; 32]))),
            cipher: Cipher::Aes256Gcm,
        }
    }

    /// FC-SEC-INV-028:信封往返一致;密文不含明文字节。
    #[test]
    fn envelope_roundtrip_hides_plaintext() {
        let encryption = encryption();
        let plaintext = b"secret memory text with key data";
        let envelope = seal(&encryption, b"vsec", 3, plaintext).expect("seal");
        assert!(is_envelope(&envelope));
        assert!(
            !envelope
                .windows(plaintext.len())
                .any(|window| window == plaintext),
            "密文不得包含明文"
        );
        let opened = open(&encryption, b"vsec", 3, &envelope).expect("open");
        assert_eq!(opened, plaintext);
    }

    /// FC-SEC-INV-028:翻转 1 bit(密文或 AAD 绑定字段)→ `Corrupted`。
    #[test]
    fn tampering_and_wrong_scope_are_rejected() {
        let encryption = encryption();
        let envelope = seal(&encryption, b"vsec", 3, b"hello").expect("seal");
        let mut flipped = envelope.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 0x01;
        assert!(matches!(
            open(&encryption, b"vsec", 3, &flipped),
            Err(MnemeError::Corrupted { .. })
        ));
        // AAD 绑定用途与标识:跨用途/跨段搬运必须失败。
        assert!(matches!(
            open(&encryption, b"msec", 3, &envelope),
            Err(MnemeError::Corrupted { .. })
        ));
        assert!(matches!(
            open(&encryption, b"vsec", 4, &envelope),
            Err(MnemeError::Corrupted { .. })
        ));
    }

    /// FC-SEC-POST-001:错误密钥/密钥缺失 → `Corrupted`;轮换后新旧密钥均可读。
    #[test]
    fn wrong_key_fails_and_rotation_reads_both() {
        let keyring = Arc::new(Keyring::new(KeyId(1), Key::from_bytes([1_u8; 32])));
        let encryption = Encryption {
            provider: Arc::clone(&keyring) as Arc<dyn KeyProvider>,
            cipher: Cipher::Aes256Gcm,
        };
        let old = seal(&encryption, b"msec", 1, b"payload").expect("seal");
        let new_id = keyring.rotate().expect("rotate");
        assert_ne!(new_id, KeyId(1));
        let new = seal(&encryption, b"msec", 1, b"payload2").expect("seal2");
        // 轮换后:旧段(旧 key_id)与新段(新 key_id)都可读。
        assert_eq!(
            open(&encryption, b"msec", 1, &old).expect("old"),
            b"payload"
        );
        assert_eq!(
            open(&encryption, b"msec", 1, &new).expect("new"),
            b"payload2"
        );
        // 退役旧密钥后旧段不可读(必须已完成迁移)。
        assert!(keyring.retire(KeyId(1)));
        assert!(matches!(
            open(&encryption, b"msec", 1, &old),
            Err(MnemeError::Corrupted { .. })
        ));
        assert_eq!(
            open(&encryption, b"msec", 1, &new).expect("new key 仍在"),
            b"payload2"
        );
    }

    /// FC-SEC-POST-001:密钥编号分配 checked,耗尽时 `IdExhausted`,
    /// 绝不静默复用同编号覆盖已有密钥。
    #[test]
    fn keyring_rotate_exhaustion_returns_id_exhausted() {
        let keyring = Keyring::new(KeyId(u32::MAX), Key::from_bytes([1_u8; 32]));
        let error = keyring.rotate().expect_err("编号耗尽必须失败");
        assert!(matches!(error, MnemeError::IdExhausted { kind: "key_id" }));
        // 失败不得改动 active(否则会覆盖 u32::MAX 处的已有密钥)。
        assert_eq!(keyring.active_key(), KeyId(u32::MAX));
    }
}
