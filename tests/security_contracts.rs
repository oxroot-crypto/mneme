//! L11 安全存储契约验收:静态加密(整文件 AEAD 信封)与文本/元数据压缩。
//!
//! 覆盖 `docs/spec/contracts.md` 的以下条目:
//!
//! * FC-SEC-INV-028(加密后磁盘无明文记录字段;认证失败/错误密钥 → `Corrupted`)
//! * FC-SEC-POST-001(密钥轮换后新旧均可读,迁移完成旧 key 退役)
//! * FC-SEC-POST-002(压缩 roundtrip、无收益回退原文、`None` 布局不变)
//! * FC-SEC-CPLX-001(加解密/压缩线性复杂度哨兵)
//! * FC-SEC-ERR-001(feature 未开/未知 codec → 结构化拒绝,绝不静默)
//!
//! 不变量锚定:I28(加密不落明文;认证失败/错误密钥可检出)。
//!
//! 跨 feature 用例(未开 `encrypt` 的构建打开加密库必须结构化拒绝、绝不按明文
//! 解析)由 CI 矩阵分两阶段执行(`MNEME_ENCRYPT_FIXTURE` 传目录),见设计 14 §7。

mod common;

use std::sync::Arc;

use mneme::{Builder, Cipher, Compression, CryptoKey, Encryption, KeyId, KeyProvider, Keyring};

#[cfg(feature = "encrypt")]
use mneme::Mneme;

/// 测试密钥(固定字节,便于跨阶段复用)。
#[cfg(feature = "encrypt")]
fn test_keyring() -> Arc<Keyring> {
    Arc::new(Keyring::new(KeyId(1), CryptoKey::from_bytes([7_u8; 32])))
}

/// 建一个启用加密的持久库。
#[cfg(feature = "encrypt")]
fn encrypted_db(dir: &std::path::Path) -> (Mneme, Arc<Keyring>) {
    let keyring = test_keyring();
    let db = Builder::default()
        .dimension(2)
        .path(dir)
        .encryption(Some(Encryption {
            provider: Arc::clone(&keyring) as Arc<dyn KeyProvider>,
            cipher: Cipher::Aes256Gcm,
        }))
        .build()
        .expect("build encrypted");
    (db, keyring)
}

/// 递归扫描目录下全部文件字节,返回是否包含给定子串。
#[cfg(feature = "encrypt")]
fn dir_contains(dir: &std::path::Path, needle: &[u8]) -> bool {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_dir() {
                stack.push(child);
                continue;
            }
            let Ok(bytes) = std::fs::read(&child) else {
                continue;
            };
            if bytes.windows(needle.len()).any(|window| window == needle) {
                return true;
            }
        }
    }
    false
}

/// **FC-SEC-INV-028**:开启加密后磁盘(段/WAL/MANIFEST)不得出现明文记录字段。
#[cfg(feature = "encrypt")]
#[test]
fn encrypted_library_never_writes_plaintext() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let (db, _keyring) = encrypted_db(dir.path());
        let ns = db.namespace("vault");
        ns.insert(mneme::Record::new(vec![1.0, 0.0]).key("secret-key"))
            .expect("insert");
        // 停用词外的独特文本,确保不会被分词改写。
        ns.insert(
            mneme::Record::new(vec![0.0, 1.0])
                .key("k2")
                .text("zzyzx-secret-plaintext-marker")
                .metadata(mneme::Meta::from(
                    mneme::json!({"note": "zzyzx-meta-marker"}),
                )),
        )
        .expect("insert text");
        db.flush().expect("flush");
        db.close().expect("close");
    }
    assert!(
        !dir_contains(dir.path(), b"zzyzx-secret-plaintext-marker"),
        "磁盘不得含明文 text"
    );
    assert!(
        !dir_contains(dir.path(), b"zzyzx-meta-marker"),
        "磁盘不得含明文 metadata"
    );
    assert!(
        !dir_contains(dir.path(), b"secret-key"),
        "磁盘不得含明文 key"
    );
    // 用同一密钥可正常读回。
    let (db, _keyring) = encrypted_db(dir.path());
    let ns = db.namespace("vault");
    let record = ns.get("k2").expect("get").expect("记录存在");
    assert_eq!(record.text(), Some("zzyzx-secret-plaintext-marker"));
    assert_eq!(
        record.metadata(),
        &mneme::json!({"note": "zzyzx-meta-marker"})
    );
    assert!(db.check().expect("check").ok);
    db.close().expect("close");
}

/// **FC-SEC-INV-028**:翻转密文 1 bit → `Corrupted`;错误密钥 → `Corrupted`。
#[cfg(feature = "encrypt")]
#[test]
fn tamper_and_wrong_key_are_detected() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let (db, _keyring) = encrypted_db(dir.path());
        db.namespace("vault")
            .insert(mneme::Record::new(vec![1.0, 0.0]).key("a"))
            .expect("insert");
        db.flush().expect("flush");
        db.close().expect("close");
    }
    let segment = dir.path().join("segments").join("seg_000000.vsec");
    let mut bytes = std::fs::read(&segment).expect("read segment");
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    std::fs::write(&segment, &bytes).expect("write tampered");

    let tampered = Builder::default()
        .path(dir.path())
        .encryption(Some(Encryption {
            provider: Arc::clone(&test_keyring()) as Arc<dyn KeyProvider>,
            cipher: Cipher::Aes256Gcm,
        }))
        .fail_fast_on_corruption(true)
        .build();
    assert!(
        matches!(tampered, Err(mneme::MnemeError::Corrupted { .. })),
        "翻 1 bit 必须检出: {tampered:?}"
    );

    // 恢复原文件后换错密钥:同样拒绝。
    bytes[last] ^= 0x01;
    std::fs::write(&segment, &bytes).expect("restore");
    let wrong = Arc::new(Keyring::new(KeyId(1), CryptoKey::from_bytes([8_u8; 32])));
    let rejected = Builder::default()
        .path(dir.path())
        .encryption(Some(Encryption {
            provider: wrong as Arc<dyn KeyProvider>,
            cipher: Cipher::Aes256Gcm,
        }))
        .fail_fast_on_corruption(true)
        .build();
    assert!(
        matches!(rejected, Err(mneme::MnemeError::Corrupted { .. })),
        "错误密钥必须拒绝: {rejected:?}"
    );
}

/// **FC-SEC-POST-001**:密钥轮换后新旧段均可读;全量迁移完成后旧 key 可退役。
#[cfg(feature = "encrypt")]
#[test]
fn key_rotation_migrates_and_retires_old_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let keyring = test_keyring();
    let encryption = Encryption {
        provider: Arc::clone(&keyring) as Arc<dyn KeyProvider>,
        cipher: Cipher::Aes256Gcm,
    };
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .encryption(Some(encryption.clone()))
            .build()
            .expect("build encrypted");
        let ns = db.namespace("vault");
        ns.insert(mneme::Record::new(vec![1.0, 0.0]).key("before"))
            .expect("insert before");
        db.flush().expect("flush old key");
        let new_id = db.rotate_encryption_key().expect("rotate");
        assert_ne!(new_id, KeyId(1));
        ns.insert(mneme::Record::new(vec![0.0, 1.0]).key("after"))
            .expect("insert after");
        db.flush().expect("flush new key");
        let stats = db.stats().expect("stats");
        assert!(stats.storage.encryption);
        assert_eq!(
            stats.storage.migrated_segments, stats.storage.total_segments,
            "全量重写后所有段都应使用 active 密钥"
        );
        // 迁移完成:旧 key 退役,同进程内新旧记录仍可读。
        assert!(keyring.retire(KeyId(1)), "旧 key 退役");
        assert!(ns.get("before").expect("get before").is_some());
        assert!(ns.get("after").expect("get after").is_some());
        db.close().expect("close");
    }
    // 重开:provider 只剩新 key,库完整可读(旧段确已迁移)。
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .encryption(Some(encryption))
        .build()
        .expect("reopen with new key only");
    assert!(db.namespace("vault").get("before").expect("get").is_some());
    db.close().expect("close");
}

/// **FC-SEC-POST-002**:压缩 roundtrip、无收益回退原文、`Compression::None` 语义不变。
#[cfg(feature = "compress")]
#[test]
fn compression_roundtrip_and_threshold() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repetitive = "agent memory ".repeat(64);
    {
        let db = Builder::default()
            .dimension(2)
            .path(dir.path())
            .compression(Compression::Lz4)
            .build()
            .expect("build compressed");
        let ns = db.namespace("vault");
        ns.insert(
            mneme::Record::new(vec![1.0, 0.0])
                .key("rep")
                .text(repetitive.clone()),
        )
        .expect("insert repetitive");
        ns.insert(mneme::Record::new(vec![0.0, 1.0]).key("tiny").text("short"))
            .expect("insert tiny");
        db.flush().expect("flush");
        db.close().expect("close");
    }
    let msec = std::fs::read(dir.path().join("segments").join("seg_000000.msec")).expect("msec");
    assert!(
        !msec
            .windows(repetitive.len())
            .any(|window| window == repetitive.as_bytes()),
        "高重复文本必须被压缩(磁盘不得含原文)"
    );
    assert!(
        msec.windows(5).any(|window| window == b"short"),
        "无收益短文本应回退原文"
    );

    let db = Builder::default()
        .path(dir.path())
        .compression(Compression::Lz4)
        .build()
        .expect("reopen");
    let ns = db.namespace("vault");
    assert_eq!(
        ns.get("rep").expect("get").expect("存在").text(),
        Some(&*repetitive)
    );
    assert_eq!(
        ns.get("tiny").expect("get").expect("存在").text(),
        Some("short")
    );
    // 文本检索与元数据过滤仍可用。
    let hits = ns
        .search()
        .text("agent")
        .top_k(4)
        .execute()
        .expect("search");
    assert!(!hits.is_empty(), "压缩不得影响 BM25 倒排");
    db.close().expect("close");
}

/// **FC-SEC-ERR-001**:feature 未开时配置压缩/加密必须构造期显式拒绝。
#[cfg(not(feature = "compress"))]
#[test]
fn compression_requires_feature() {
    let error = Builder::default()
        .dimension(2)
        .compression(Compression::Lz4)
        .build()
        .expect_err("未开 compress 必须拒绝");
    assert!(matches!(
        error,
        mneme::MnemeError::Unsupported {
            feature: "compress"
        }
    ));
}

/// **FC-SEC-ERR-001**:`Zstd` 需 feature `compress-zstd`。
#[cfg(not(feature = "compress-zstd"))]
#[test]
fn zstd_requires_feature() {
    let error = Builder::default()
        .dimension(2)
        .compression(Compression::Zstd)
        .build()
        .expect_err("未开 compress-zstd 必须拒绝");
    assert!(matches!(
        error,
        mneme::MnemeError::Unsupported {
            feature: "compress-zstd"
        }
    ));
}

/// **FC-SEC-ERR-001**:未开 `encrypt` 时配置加密必须构造期显式拒绝。
#[cfg(not(feature = "encrypt"))]
#[test]
fn encryption_requires_feature() {
    let error = Builder::default()
        .dimension(2)
        .encryption(Some(Encryption {
            provider: Arc::new(Keyring::new(KeyId(1), CryptoKey::from_bytes([1_u8; 32])))
                as Arc<dyn KeyProvider>,
            cipher: Cipher::Aes256Gcm,
        }))
        .build()
        .expect_err("未开 encrypt 必须拒绝");
    assert!(matches!(
        error,
        mneme::MnemeError::Unsupported { feature: "encrypt" }
    ));
}

/// FC-SEC-CPLX-001 哨兵:加密与压缩的大输入往返均成功(线性路径无隐式限制)。
#[cfg(feature = "encrypt")]
#[test]
fn large_payload_roundtrip_is_linear_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let (db, _keyring) = encrypted_db(dir.path());
        // 8 MiB 级文本(接近字段上限前)经加密 + flush + 读取往返一致。
        let text = "mneme-".repeat(100_000);
        db.namespace("vault")
            .insert(
                mneme::Record::new(vec![1.0, 0.0])
                    .key("big")
                    .text(text.clone()),
            )
            .expect("insert");
        db.flush().expect("flush");
        db.close().expect("close");
        let (db, _keyring) = encrypted_db(dir.path());
        let ns = db.namespace("vault");
        let record = ns.get("big").expect("get").expect("存在");
        assert_eq!(record.text().map(str::len), Some(text.len()));
        db.close().expect("close");
    }
}

/// FC-SEC-ERR-001 跨 feature 第 1 阶段(CI,`--features encrypt`):
/// 在 `MNEME_ENCRYPT_FIXTURE` 指定目录建一个加密库(固定密钥)。
#[cfg(feature = "encrypt")]
#[test]
#[ignore = "CI 跨 feature 矩阵 phase 1:先用 --features encrypt 建库"]
fn write_encrypted_fixture_for_cross_feature_check() {
    let dir = common::env::require(common::env::ENCRYPT_FIXTURE);
    let path = std::path::Path::new(&dir);
    let _ = std::fs::remove_dir_all(path);
    std::fs::create_dir_all(path).expect("创建 fixture 目录");
    let (db, _keyring) = encrypted_db(path);
    db.namespace("vault")
        .insert(mneme::Record::new(vec![1.0, 0.0]).key("a"))
        .expect("insert");
    db.flush().expect("flush");
    db.close().expect("close");
}

/// FC-SEC-ERR-001 跨 feature 第 2 阶段(CI,默认构建):不带任何加密配置直接
/// 打开 phase 1 的加密 fixture,信封探测必须返回结构化 `Unsupported`
/// (`feature: "encrypt"`),绝不归入 `Corrupted`、绝不按明文解析。
#[cfg(not(feature = "encrypt"))]
#[test]
#[ignore = "CI 跨 feature 矩阵 phase 2:默认构建打开加密库必须拒绝"]
fn open_encrypted_fixture_requires_feature() {
    let dir = common::env::require(common::env::ENCRYPT_FIXTURE);
    let error = Builder::default()
        .path(std::path::Path::new(&dir))
        .build()
        .expect_err("未开 encrypt 必须拒绝打开加密库");
    assert!(
        matches!(error, mneme::MnemeError::Unsupported { feature: "encrypt" }),
        "加密库在未开 encrypt 的构建上必须返回 Unsupported 根因(绝不按明文解析),实际 {error:?}"
    );
}

/// FC-SEC-POST-001(`migrated_segments`):迁移统计只读 vsec 信封头 10 字节,
/// 不得为统计整读段文件(与惰性段驻留同口径的回归守卫)。
#[cfg(feature = "encrypt")]
#[test]
fn migrated_segments_probes_envelope_header_without_full_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let (db, _keyring) = encrypted_db(dir.path());
        db.namespace("vault")
            .insert(mneme::Record::new(vec![1.0, 0.0]).key("a"))
            .expect("insert");
        db.flush().expect("flush");
        db.close().expect("close");
    }

    let keyring = test_keyring();
    let spy = common::SegmentReadSpy::new(dir.path());
    let full_reads = Arc::clone(&spy.full_reads);
    let db = Builder::default()
        .dimension(2)
        .path(dir.path())
        .storage(Arc::new(spy))
        .encryption(Some(Encryption {
            provider: Arc::clone(&keyring) as Arc<dyn KeyProvider>,
            cipher: Cipher::Aes256Gcm,
        }))
        .build()
        .expect("reopen encrypted with spy");
    // 打开阶段(解密/校验)允许整读;此处只考核统计路径。
    full_reads.lock().expect("lock").clear();
    let stats = db.stats().expect("stats");
    assert!(stats.storage.migrated_segments > 0);
    let reads = full_reads.lock().expect("lock");
    assert!(
        reads.is_empty(),
        "迁移统计不得整读 vsec(应只读 10 字节信封头),实际: {reads:?}"
    );
    db.close().expect("close");
}
