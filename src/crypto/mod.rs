//! 静态加密(L11,设计 11 §2):AES-256-GCM 整文件信封。
//!
//! 威胁模型只保护**静态介质**:密文文件为自描述信封
//! `[MNEC][u16 格式版本][u32 key_id][u32 明文长度][12B nonce][密文][16B tag]`,
//! AAD 绑定 `(用途, 段号/版本, 格式版本)` 防跨文件搬运;密钥经 [`KeyProvider`]
//! 注入,引擎不管理密钥文件。开启加密的段以整文件解密进入自有缓冲(设计取舍:
//! mmap 零拷贝对加密段失效),解密失败/错误密钥一律 `Corrupted`,绝不返回错误
//! 数据(不变量 I28 的加密版)。

mod envelope;
mod file;
mod key;

#[cfg(all(test, feature = "encrypt"))]
mod tests;

pub use key::{Cipher, Encryption, Key, KeyId, KeyProvider, Keyring};

pub(crate) use envelope::{ENVELOPE_MAGIC, envelope_key_id, is_envelope, open, seal};
pub(crate) use file::{decrypt_file, encrypt_file, ensure_supported};
