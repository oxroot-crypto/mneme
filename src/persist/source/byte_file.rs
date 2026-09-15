//! 段文件只读句柄:`ByteFile` 与其后端(`mmap` 映射或整文件读入)。

use std::sync::Arc;

use crate::core::error::Result;
use crate::memory::lazy::ByteSource;
use crate::persist::storage::{SEGMENTS_DIR, Storage};

/// 单个段文件的只读句柄(mmap 零拷贝或整文件读入)。
///
/// 段文件 write-once;持有 `Arc<ByteFile>` 即保证文件内容可读——compaction
/// 把旧段移入 `trash/`(或下次打开物理删除)只影响目录项,已打开的描述符与
/// mmap 映射在句柄存活期内继续有效(POSIX unlink/rename 语义,FC-PERSIST-INV-021)。
#[derive(Debug)]
pub(crate) struct ByteFile {
    /// 所属段编号(诊断用)。
    segment_id: u32,
    /// 存储后端。
    backend: Backend,
    /// 文件字节数(避免每次取切片都查元数据)。
    len: usize,
}

/// [`ByteFile`] 的后端实现。
#[derive(Debug)]
enum Backend {
    /// `feature = "mmap"`(默认):内核按页惰性载入的只读映射。
    #[cfg(all(feature = "mmap", not(feature = "wasm")))]
    Mmap(memmap2::Mmap),
    /// 非 mmap 构建:整文件读入自有缓冲(功能等价,设计 04 §11);
    /// mmap 构建下仅单元测试(`from_bytes`)构造。
    // reason: mmap 构建的生产路径恒为 `Mmap`;该变体保留双后端形状与测试入口。
    #[cfg_attr(all(feature = "mmap", not(feature = "wasm")), allow(dead_code))]
    Owned(Box<[u8]>),
}

/// [`ByteFile::open`] 的输入参数。
pub(crate) struct ByteFileOpenInput<'a> {
    /// 存储后端。
    pub(crate) storage: &'a Arc<dyn Storage>,
    /// 段编号(加密信封身份)。
    pub(crate) segment_id: u32,
    /// 加密信封作用域(如 `b"vsec"`)。
    pub(crate) scope: &'a [u8],
    /// 文件相对名。
    pub(crate) name: &'a str,
    /// 静态加密配置(`None` = 明文)。
    pub(crate) encryption: Option<&'a crate::crypto::Encryption>,
}

impl ByteFile {
    /// 只读打开 `segments/<name>`。
    ///
    /// # Errors
    /// 文件缺失/权限不足/映射失败时返回底层 [`std::io::Error`]。
    pub(crate) fn open(input: &ByteFileOpenInput<'_>) -> Result<Arc<Self>> {
        let ByteFileOpenInput {
            storage,
            segment_id,
            scope,
            name,
            encryption,
        } = *input;
        let rel = format!("{SEGMENTS_DIR}/{name}");
        // 信封探测只读前 4 字节;非加密段直接进入 mmap/按需读取路径,不得为
        // 探测整读大段(FC-PERSIST-INV-021 / FC-PERSIST-CPLX-007)。
        let head = storage.read_prefix(&rel, crate::crypto::ENVELOPE_MAGIC.len())?;
        if head == crate::crypto::ENVELOPE_MAGIC {
            let probe = storage.read_file(&rel)?;
            let plain =
                crate::crypto::decrypt_file(encryption, scope, u64::from(segment_id), probe)?;
            let len = plain.len();
            return Ok(Arc::new(Self {
                segment_id,
                backend: Backend::Owned(plain.into_boxed_slice()),
                len,
            }));
        }
        let raw = storage.open_bytes(&rel)?;
        let len = raw.len();
        Ok(Arc::new(Self {
            segment_id,
            backend: match raw {
                #[cfg(all(feature = "mmap", not(feature = "wasm")))]
                crate::persist::storage::RawBytes::Mmap(map) => Backend::Mmap(map),
                crate::persist::storage::RawBytes::Owned(bytes) => Backend::Owned(bytes),
            },
            len,
        }))
    }

    /// 以自有字节构造(单元测试与内存态路径;不与磁盘回收关联)。
    #[cfg(test)]
    pub(crate) fn from_bytes(segment_id: u32, bytes: Vec<u8>) -> Arc<Self> {
        let len = bytes.len();
        Arc::new(Self {
            segment_id,
            backend: Backend::Owned(bytes.into_boxed_slice()),
            len,
        })
    }

    /// 文件字节数。
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// 所属段编号(诊断用)。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn segment_id(&self) -> u32 {
        self.segment_id
    }
}

impl ByteSource for ByteFile {
    fn slice_at(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        match &self.backend {
            #[cfg(all(feature = "mmap", not(feature = "wasm")))]
            Backend::Mmap(map) => map.get(offset..end),
            Backend::Owned(bytes) => bytes.get(offset..end),
        }
    }

    fn byte_len(&self) -> usize {
        self.len
    }
}
