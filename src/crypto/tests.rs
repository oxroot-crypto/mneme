//! `crypto` 信封与密钥环个单元测试(FC-SEC-INV-028、FC-SEC-POST-001)。

use std::sync::Arc;

use crate::core::error::MnemeError;

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
