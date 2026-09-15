//! WAL 写入器的追加、同步、轮转与 Checkpoint 重置。
//!
//! 文件命名与序号解析见 [`super::paths`],打开参数见 [`super::config`]。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::FsyncPolicy;
use crate::persist::hook::{FsyncHook, IoAction};
use crate::persist::storage::Storage;
use crate::persist::wal;

use super::config::WalConfig;
use super::inputs::FromPartsInput;
use super::lifecycle::create_truncating;
use super::paths::{wal_files, wal_name};

// 类型与方法可见性用 `pub(in crate::persist::store)` 精确等价原单文件里的
// `pub(super)`(原父模块即 `store`);`WalWriter` 名字仍经 `mod.rs` 重导出。
/// WAL 写入器(单活动文件;轮转后序号递增;字节操作全部经 [`Storage`])。
pub(in crate::persist::store) struct WalWriter {
    storage: Arc<dyn Storage>,
    active_index: u32,
    /// 活动文件相对路径(轮转/重建时同步更新,免每次追加都重新格式化)。
    active_rel: String,
    /// 活动文件当前字节数(本写入器是唯一写者;追加/回滚/重建处同步维护)。
    ///
    /// `pub(super)` 仅供同目录 `tests` 读取;可见性从私有放开到 `wal_writer` 内。
    pub(super) written: u64,
    /// 已轮转旧文件的字节数合计(轮转时累加、Checkpoint 删除旧文件后清零)。
    ///
    /// 供 `Store::wal_bytes` 免列目录 + stat 直接取总量(统计为尽力而为)。
    sealed_bytes: u64,
    max_file_bytes: u64,
    policy: FsyncPolicy,
    dimension: u32,
    metric: Metric,
    hook: Option<Arc<dyn FsyncHook>>,
    encryption: Option<crate::crypto::Encryption>,
    /// 单帧负载上限(字节;`0` = 不限制)。
    frame_max: usize,
    /// 只读实例(无写权限)。
    writable: bool,
    /// 重置失败且重建失败后停用:后续写入报 `Io`,绝不向状态可疑的文件追加
    /// (否则重启截断会丢已确认帧)。
    poisoned: bool,
}

impl WalWriter {
    // `pub(super)` 供 `lifecycle` 子模块的打开/轮转路径复用;拆分不改变 `wal_writer` 外可达性。
    /// 由后端与配置组装写入器;`written` 为活动文件当前字节数,
    /// `sealed` 为已轮转旧文件字节合计。
    pub(super) fn from_parts(input: FromPartsInput) -> Self {
        Self {
            storage: input.storage,
            active_index: input.active_index,
            active_rel: wal_name(input.active_index),
            written: input.written,
            sealed_bytes: input.sealed,
            max_file_bytes: input.config.max_file_bytes,
            policy: input.config.policy,
            dimension: input.config.dimension,
            metric: input.config.metric,
            hook: input.config.hook,
            encryption: input.config.encryption,
            frame_max: input.config.frame_max,
            writable: input.writable,
            poisoned: false,
        }
    }

    /// 当前活动文件相对路径。
    pub(in crate::persist::store) fn active_rel(&self) -> &str {
        &self.active_rel
    }

    /// 写前置检查:只读 → `Unsupported`;已停用 → `Io`。
    fn ensure_writable(&self) -> Result<()> {
        if self.poisoned {
            return Err(MnemeError::Io(std::io::Error::other(
                "WAL 重置失败,句柄已停用",
            )));
        }
        if !self.writable {
            return Err(MnemeError::Unsupported {
                feature: "只读模式写入",
            });
        }
        Ok(())
    }

    /// 追加一帧(不 fsync;由 [`WalWriter::sync`] 在事务末统一落盘)。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(in crate::persist::store) fn append(
        &mut self,
        seqno: u64,
        kind: wal::FrameKind,
        payload: &[u8],
    ) -> Result<()> {
        self.ensure_writable()?;
        let sealed = match self.encryption.as_ref() {
            Some(encryption) => Some(crate::crypto::seal(encryption, b"wal", seqno, payload)?),
            None => None,
        };
        // 帧负载上限(FC-PERSIST-ERR-013):按实际写入帧的字节计(加密时为信封后),
        // 超限即拒绝并交由写事务回滚,绝不截断或写出超限帧。
        let frame_payload = sealed.as_deref().unwrap_or(payload);
        if self.frame_max > 0 && frame_payload.len() > self.frame_max {
            return Err(MnemeError::LimitExceeded {
                field: "wal 帧负载",
                limit: self.frame_max,
                got: frame_payload.len(),
            });
        }
        let frame = wal::encode_frame(seqno, kind, frame_payload);
        if let Some(hook) = &self.hook {
            hook.before(IoAction::Write {
                file: &self.active_rel,
                offset: self.written,
                len: frame.len(),
            })?;
        }
        self.written = self.storage.append(&self.active_rel, &frame)?;
        Ok(())
    }

    /// 按 `FsyncPolicy` 落盘:一个写事务只调用一次(组提交,FC-PERSIST-CPLX-001);
    /// 事务末检查是否需要轮转(单批绝不跨文件)。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];同步 I/O 失败返回 [`MnemeError::Io`]。
    pub(in crate::persist::store) fn sync(&mut self) -> Result<()> {
        if self.writable && matches!(self.policy, FsyncPolicy::Always | FsyncPolicy::Batched(_)) {
            if let Some(hook) = &self.hook {
                hook.before(IoAction::Fsync {
                    file: &self.active_rel,
                })?;
            }
            self.storage.sync(&self.active_rel)?;
        }
        self.rotate_if_needed()
    }

    /// 活动文件达到轮转阈值时,在下个事务开始前切换到新文件。
    fn rotate_if_needed(&mut self) -> Result<()> {
        if self.max_file_bytes == 0 || !self.writable {
            return Ok(());
        }
        if self.written < self.max_file_bytes {
            return Ok(());
        }
        let next = self.active_index.saturating_add(1);
        let writer = create_truncating(
            Arc::clone(&self.storage),
            next,
            WalConfig {
                dimension: self.dimension,
                metric: self.metric,
                policy: self.policy,
                max_file_bytes: self.max_file_bytes,
                hook: self.hook.clone(),
                read_only: false,
                encryption: None,
                frame_max: self.frame_max,
            },
        )?;
        // 轮转:旧活动文件计入已封印字节,新写入器从文件头开始。
        let sealed = self.sealed_bytes.saturating_add(self.written);
        *self = writer;
        self.sealed_bytes = sealed;
        Ok(())
    }

    /// 当前 WAL 文件集总字节数(活动 + 已轮转;统计为尽力而为)。
    pub(in crate::persist::store) fn total_bytes(&self) -> u64 {
        self.sealed_bytes.saturating_add(self.written)
    }

    /// 当前活动 WAL 文件字节长度(用于写事务失败时回滚截断点)。
    ///
    /// 缓存值由写入器在追加/回滚/重建处同步维护;本写入器是活动文件的唯一写者。
    ///
    /// # Errors
    /// 保留 `Result` 形态与读写失败调用点兼容;当前实现不产生错误。
    pub(in crate::persist::store) fn len(&self) -> Result<u64> {
        Ok(self.written)
    }

    /// 回滚到 `len`:截断活动文件,丢弃本次事务的半写/未确认帧。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(in crate::persist::store) fn rollback_to(&mut self, len: u64) -> Result<()> {
        self.ensure_writable()?;
        self.storage.truncate(&self.active_rel, len)?;
        self.written = len;
        if let Some(hook) = &self.hook {
            hook.before(IoAction::Fsync {
                file: &self.active_rel,
            })?;
        }
        self.storage.sync(&self.active_rel)?;
        Ok(())
    }

    /// 重置/Checkpoint:重写文件头、截断活动文件并删除已轮转旧文件。
    ///
    /// 调用方保证所有 `seqno ≤ manifest.watermark` 的帧已被段/delta 覆盖;
    /// 旧文件即使残留,恢复时也会因 `seqno ≤ watermark` 被跳过。
    ///
    /// 实现按"先原子换头、后截断"顺序,任何时刻文件头都完整;任一步失败时尝试原地
    /// 重建,重建仍失败才停用句柄——绝不向可疑文件追加(否则重启截断会丢已确认帧,
    /// FC-PERSIST-INV-005)。
    ///
    /// # Errors
    /// 只读实例返回 [`MnemeError::Unsupported`];I/O 失败返回 [`MnemeError::Io`]。
    pub(in crate::persist::store) fn reset(&mut self) -> Result<()> {
        if let Err(error) = self.rewrite_header_and_truncate() {
            // 重建兜底:同样经过 hook(注入失败必须让 writer 停用,绝不向可疑文件追加)。
            let header = wal::encode_file_header(self.dimension, self.metric);
            let rebuild = (|| -> Result<()> {
                if let Some(hook) = &self.hook {
                    hook.before(IoAction::Write {
                        file: &self.active_rel,
                        offset: 0,
                        len: header.len(),
                    })?;
                }
                self.storage.write_atomic(&self.active_rel, &header)?;
                if let Some(hook) = &self.hook {
                    hook.before(IoAction::Fsync {
                        file: &self.active_rel,
                    })?;
                }
                self.storage.sync(&self.active_rel)?;
                Ok(())
            })();
            if rebuild.is_ok() {
                self.written = header.len() as u64;
            } else {
                self.poisoned = true;
            }
            return Err(error);
        }
        let active = self.active_rel();
        for rel in wal_files(self.storage.as_ref())? {
            if rel != active {
                // reason: 旧轮转文件即使残留,恢复时也因 seqno ≤ watermark 被跳过,
                // 删除失败不影响正确性。
                let _ = self.storage.remove_if_exists(&rel).ok();
            }
        }
        // Checkpoint 成功:旧轮转文件的字节不再计入 WAL 总量。
        self.sealed_bytes = 0;
        Ok(())
    }

    /// 重置的第一步:原子换头(写头 + fsync)并把活动文件截到头部长度。
    fn rewrite_header_and_truncate(&mut self) -> Result<()> {
        self.ensure_writable()?;
        let header = wal::encode_file_header(self.dimension, self.metric);
        if let Some(hook) = &self.hook {
            hook.before(IoAction::Write {
                file: &self.active_rel,
                offset: 0,
                len: header.len(),
            })?;
        }
        self.storage.write_atomic(&self.active_rel, &header)?;
        self.storage
            .truncate(&self.active_rel, wal::FILE_HEADER_LEN as u64)?;
        self.written = wal::FILE_HEADER_LEN as u64;
        if let Some(hook) = &self.hook {
            hook.before(IoAction::Fsync {
                file: &self.active_rel,
            })?;
        }
        self.storage.sync(&self.active_rel)?;
        Ok(())
    }
}
