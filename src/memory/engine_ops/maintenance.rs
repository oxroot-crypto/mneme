//! 运维方法:只读重载、加密密钥轮换与显式落盘。

use crate::core::error::{MnemeError, Result};
use crate::memory::engine::Mneme;

impl Mneme {
    /// 只读实例重载:发现更新的已提交 MANIFEST 时原子切换视图。
    ///
    /// # Returns
    /// 切换后的新 MANIFEST 版本号;无新版本时 `None`(旧视图保持)。
    ///
    /// # Errors
    /// 纯内存库/可写实例调用返回结构化错误;新版本损坏时保持旧视图并返回错误。
    ///
    /// # Examples
    /// ```
    /// use std::sync::Arc;
    ///
    /// use mneme::{Builder, MemStorage, Record, Storage};
    ///
    /// let storage: Arc<dyn Storage> = Arc::new(MemStorage::new());
    /// let writer = Builder::default()
    ///     .path("mem://reload-doc").storage(Arc::clone(&storage))
    ///     .dimension(2).maintenance(false).build().unwrap();
    /// writer.namespace("n").insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// writer.flush().unwrap();
    /// // 关闭只读实例自动探测,示例只验证显式 `reload`。
    /// let reader = Builder::default()
    ///     .path("mem://reload-doc").storage(Arc::clone(&storage))
    ///     .read_only(true).read_only_probe_interval(std::time::Duration::ZERO)
    ///     .build().unwrap();
    /// writer.namespace("n").insert(Record::new(vec![0.0, 1.0]).key("b")).unwrap();
    /// writer.flush().unwrap();
    /// assert!(reader.reload().unwrap().is_some());
    /// ```
    pub fn reload(&self) -> Result<Option<u64>> {
        let Some(store) = &self.store else {
            return Err(MnemeError::Unsupported {
                feature: "纯内存库 reload",
            });
        };
        if !store.read_only {
            return Err(MnemeError::Config {
                reason: "仅只读实例支持 reload",
            });
        }
        match crate::persist::store::reload_read_only(store)? {
            Some((state, version)) => {
                self.table.publish(&state);
                Ok(Some(version))
            }
            None => Ok(None),
        }
    }

    /// 轮换静态加密密钥:`provider.rotate()` 后全量重写段与 MANIFEST。
    ///
    /// 迁移期间新旧密钥均可读(`KeyProvider` 需同时持有两者);全部段迁移完成后
    /// 宿主可退役旧密钥(`Keyring::retire`)。纯内存库/未启用加密 → 结构化拒绝。
    ///
    /// # Returns
    /// 新的 active 密钥标识。
    ///
    /// # Errors
    /// 未启用加密、provider 未实现轮换、提交/写入失败时返回结构化错误
    /// (`FC-SEC-POST-001`)。
    ///
    /// # Examples
    /// ```
    /// # #[cfg(feature = "encrypt")]
    /// # {
    /// use std::sync::Arc;
    ///
    /// use mneme::{Builder, Cipher, CryptoKey, Encryption, KeyId, KeyProvider, Keyring, Record};
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let keyring = Arc::new(Keyring::new(KeyId(1), CryptoKey::from_bytes([7_u8; 32])));
    /// let encryption = Encryption {
    ///     provider: Arc::clone(&keyring) as Arc<dyn KeyProvider>,
    ///     cipher: Cipher::Aes256Gcm,
    /// };
    /// let db = Builder::default()
    ///     .path(dir.path())
    ///     .dimension(2).encryption(Some(encryption)).build().unwrap();
    /// let ns = db.namespace("vault");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// db.flush().unwrap();
    /// // 轮换后新 id 单调递增,迁移完成前旧记录经 provider 双密钥仍可读。
    /// assert_eq!(db.rotate_encryption_key().unwrap(), KeyId(2));
    /// assert!(ns.get("a").unwrap().is_some());
    /// # }
    /// ```
    pub fn rotate_encryption_key(&self) -> Result<crate::KeyId> {
        let Some(store) = &self.store else {
            return Err(MnemeError::Unsupported {
                feature: "纯内存库密钥轮换",
            });
        };
        let Some(encryption) = store.encryption_config().cloned() else {
            return Err(MnemeError::Config {
                reason: "库未启用加密,无密钥可轮换",
            });
        };
        let new_id = encryption.provider.rotate()?;
        self.rewrite_all_segments()?;
        Ok(new_id)
    }

    /// 显式落盘:把未落盘槽位与自上次 flush 的 delta 物化为新段并提交 MANIFEST。
    ///
    /// 纯内存库为空操作;持久库执行增量段 flush + WAL Checkpoint(设计 04 §3.2、
    /// 07 §4);无新增且无 delta 时为空操作。
    ///
    /// # Returns
    /// 落盘完成(含空操作)返回 `Ok(())`。
    ///
    /// # Errors
    /// 库已关闭时返回 [`MnemeError::Closed`];只读模式返回
    /// [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Mneme, Record};
    ///
    /// let db = Mneme::in_memory(2).unwrap();
    /// let ns = db.namespace("demo");
    /// ns.insert(Record::new(vec![1.0, 0.0]).key("a")).unwrap();
    /// // 纯内存库上 flush 是空操作,数据仍在且返回 `Ok(())`。
    /// db.flush().unwrap();
    /// assert!(ns.exists("a").unwrap());
    /// ```
    pub fn flush(&self) -> Result<()> {
        let view = self.table.view();
        if view.closed {
            return Err(MnemeError::Closed);
        }
        drop(view);
        if let Some(store) = &self.store {
            let mut ws = self.table.write();
            if ws.closed {
                return Err(MnemeError::Closed);
            }
            store.flush(&mut ws, &self.config)?;
            self.table.publish(&ws);
        }
        Ok(())
    }
}
