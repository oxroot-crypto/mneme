//! 密钥材料、AEAD 算法与密钥提供者抽象(设计 11 §2)。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::core::error::{MnemeError, Result};

/// AES-256 密钥长度。
const KEY_LEN: usize = 32;

/// 密钥标识:写入信封头,解密时按 id 向 provider 取密钥。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyId(pub u32);

/// 32 字节对称密钥(刻意不实现会打印明文内容的 `Debug`)。
#[derive(Clone, PartialEq, Eq)]
pub struct Key(pub(super) [u8; KEY_LEN]);

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
    ///
    /// # Examples
    /// ```
    /// # #[cfg(feature = "encrypt")]
    /// # {
    /// use mneme::CryptoKey;
    ///
    /// let first = CryptoKey::generate().unwrap();
    /// let second = CryptoKey::generate().unwrap();
    /// assert_ne!(first, second, "每次生成独立随机密钥");
    /// assert_eq!(format!("{first:?}"), "Key(<redacted>)");
    /// # }
    /// ```
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
