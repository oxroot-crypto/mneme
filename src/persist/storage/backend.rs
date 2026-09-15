//! 存储后端抽象(`Storage`)与只读字节视图(`RawBytes`)。

use crate::core::error::Result;

/// 文件元数据(不依赖 `std::fs`,宿主后端同样可实现;设计 12 §3.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    /// 文件字节数。
    pub len: u64,
}

/// 存储后端抽象(根目录绑定;路径均为库内相对路径,见设计 12 §3.1)。
///
/// 桌面/服务器默认 [`FsStorage`](crate::persist::storage::FsStorage);WASM/宿主可注入 [`MemStorage`](crate::persist::storage::MemStorage) 或自定义后端
/// (`Builder::storage`)。文件锁在 trait 内提供等价能力,锁文件/目录语义由实现
/// 负责;写路径的原子提交(`write_atomic` = 临时文件 + rename + fsync)是后端契约。
pub trait Storage: Send + Sync + std::fmt::Debug {
    /// 读取整个文件。
    ///
    /// # Errors
    /// 文件不存在或 I/O 失败时返回结构化错误。
    fn read_file(&self, rel: &str) -> Result<Vec<u8>>;

    /// 读取整个文件;不存在返回 `None`。
    ///
    /// # Errors
    /// 其他 I/O 失败返回结构化错误。
    fn read_file_opt(&self, rel: &str) -> Result<Option<Vec<u8>>>;

    /// 读取文件前缀(至多 `max` 字节;文件更短时返回全部)。
    ///
    /// 段打开仅需前 4 字节做信封探测,不得为此整读大段(FC-PERSIST-INV-021);
    /// 默认实现经 [`Storage::read_file`] 整读后截断(内存后端语义正确),
    /// 文件后端应覆写为真正的部分读取。
    ///
    /// # Errors
    /// 文件不存在或 I/O 失败时返回结构化错误。
    fn read_prefix(&self, rel: &str, max: usize) -> Result<Vec<u8>> {
        let mut bytes = self.read_file(rel)?;
        bytes.truncate(max);
        Ok(bytes)
    }

    /// 原子写入:临时文件 → fsync → rename → 目录 fsync(绝不原地覆盖)。
    ///
    /// # Errors
    /// 任一步 I/O 失败时返回结构化错误。
    fn write_atomic(&self, rel: &str, bytes: &[u8]) -> Result<()>;

    /// 只创建不覆盖地写入(目标已存在则失败)。
    ///
    /// # Errors
    /// 目标已存在或 I/O 失败时返回结构化错误。
    fn write_new(&self, rel: &str, bytes: &[u8]) -> Result<()>;

    /// 追加写入并返回追加后的文件长度。
    ///
    /// # Errors
    /// I/O 失败时返回结构化错误。
    fn append(&self, rel: &str, bytes: &[u8]) -> Result<u64>;

    /// 把文件截断到 `len` 字节并尽力 fsync。
    ///
    /// # Errors
    /// 文件不存在或 I/O 失败时返回结构化错误。
    fn truncate(&self, rel: &str, len: u64) -> Result<()>;

    /// 把文件 fsync 到稳定存储。
    ///
    /// # Errors
    /// I/O 失败时返回结构化错误。
    fn sync(&self, rel: &str) -> Result<()>;

    /// 列出目录下的文件名(不含子目录),目录不存在返回空。
    ///
    /// # Errors
    /// I/O 失败时返回结构化错误。
    fn list_dir(&self, rel: &str) -> Result<Vec<String>>;

    /// 创建目录(幂等)。
    ///
    /// # Errors
    /// I/O 失败时返回结构化错误。
    fn ensure_dir(&self, rel: &str) -> Result<()>;

    /// 删除文件;不存在视为成功。
    ///
    /// # Errors
    /// 其他 I/O 失败返回结构化错误。
    fn remove_if_exists(&self, rel: &str) -> Result<()>;

    /// 重命名文件(同后端内)。
    ///
    /// # Errors
    /// I/O 失败时返回结构化错误。
    fn rename(&self, from: &str, to: &str) -> Result<()>;

    /// 文件是否存在。
    ///
    /// # Errors
    /// 查询失败时返回结构化错误。
    fn exists(&self, rel: &str) -> Result<bool>;

    /// 文件元数据。
    ///
    /// # Errors
    /// 文件不存在或查询失败时返回结构化错误。
    fn stat(&self, rel: &str) -> Result<FileMeta>;

    /// 以只读视图打开文件:支持零拷贝的后端可返回映射,否则返回整文件自有缓冲。
    /// 默认实现 = [`Storage::read_file`](实现者不必关心 mmap)。
    ///
    /// # 前置条件(返回 `RawBytes::Mmap` 的后端)
    /// 视图存活期内文件不得被原地改写或截断;库内仅对 **write-once 段文件**
    /// 调用本方法(段经临时文件 + rename 生成,compaction 只重命名/删除目录项)。
    /// 对 `wal` / `current` / `MANIFEST.*` 等会被追加或截断的文件返回映射,
    /// 存在 `SIGBUS` 风险,自定义后端必须改为返回 `RawBytes::Owned`。
    ///
    /// # Errors
    /// 读取失败时返回结构化错误。
    fn open_bytes(&self, rel: &str) -> Result<RawBytes> {
        Ok(RawBytes::Owned(self.read_file(rel)?.into_boxed_slice()))
    }

    /// 库根目录是否存在(只读打开的前置检查;内存后端恒 `true`)。
    ///
    /// # Errors
    /// 查询失败时返回结构化错误。
    fn root_exists(&self) -> Result<bool> {
        Ok(true)
    }

    /// 尝试获取锁文件(`LOCK`)的独占锁。
    ///
    /// # Returns
    /// 成功时返回不透明守卫;`Drop` 即释放。锁被活实例持有 →
    /// [`MnemeError::Busy`](crate::core::error::MnemeError::Busy)。
    ///
    /// # Errors
    /// 见上;I/O 失败返回 [`MnemeError::Io`](crate::core::error::MnemeError::Io)。
    fn try_lock(&self) -> Result<Box<dyn std::any::Any + Send + Sync>>;
}

/// [`Storage::open_bytes`] 的只读字节视图。
#[derive(Debug)]
pub enum RawBytes {
    /// 内核按页惰性载入的只读映射(feature `mmap`)。
    #[cfg(all(feature = "mmap", not(feature = "wasm")))]
    Mmap(memmap2::Mmap),
    /// 整文件自有缓冲。
    Owned(Box<[u8]>),
}

impl RawBytes {
    /// 只读字节切片。
    ///
    /// # Returns
    /// 覆盖整个文件的连续只读字节;`mmap` 后端下由内核按页惰性载入。
    pub fn as_slice(&self) -> &[u8] {
        match self {
            #[cfg(all(feature = "mmap", not(feature = "wasm")))]
            Self::Mmap(map) => map,
            Self::Owned(bytes) => bytes,
        }
    }

    /// 字节数。
    ///
    /// # Returns
    /// 文件长度(字节)。
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// 是否为零字节。
    ///
    /// # Returns
    /// 长度为零时返回 `true`。
    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
}
