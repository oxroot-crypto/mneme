//! 段读取后端:`SegmentSource` 抽象、`FileSource`/`MmapSource` 实现与整段读取辅助。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Mutex;

/// 只读段数据源:支持整段切片(可选)与按偏移读取。
///
/// 实现者须保证 `Send + Sync` 且读取线程安全;`slice` 返回 `Some` 时调用方
/// 可直接零拷贝访问整段字节(`MmapSource`,L3),返回 `None` 时走 `read_at`。
pub(crate) trait SegmentSource: Send + Sync {
    /// 若后端支持零拷贝,返回整段字节切片;否则返回 `None`。
    // reason: 关闭 `mmap`(或 WASM 目标)时无零拷贝后端,`slice` 仅保留接口形状
    // (设计 04 §11、12 §3.2)。
    #[cfg_attr(any(not(feature = "mmap"), feature = "wasm"), allow(dead_code))]
    fn slice(&self) -> Option<&[u8]>;

    /// 从偏移 `off` 起读满 `buf`。
    ///
    /// # Errors
    /// 底层文件读取失败或不足 `buf.len()` 字节时返回 [`std::io::Error`]。
    // reason: 启用 `mmap` 时生产路径走零拷贝 `slice`,该回退读法仅 `FileSource` 使用。
    #[cfg_attr(all(feature = "mmap", not(feature = "wasm")), allow(dead_code))]
    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<()>;
}

/// 基于 `std::fs::File` 的段数据源(`seek + read`)。
///
/// 文件句柄经 [`Mutex`] 串行化,满足 `Sync`;段文件不可变,故无写竞争。
// reason: 启用 `mmap` 时生产路径走 `MmapSource`,`FileSource` 保留为显式回退后端
// (设计 04 §11);`--no-default-features`(关闭 feature `mmap`)下启用。
#[cfg_attr(all(feature = "mmap", not(feature = "wasm")), allow(dead_code))]
pub(crate) struct FileSource {
    file: Mutex<File>,
}

#[cfg_attr(all(feature = "mmap", not(feature = "wasm")), allow(dead_code))]
impl FileSource {
    /// 只读打开指定路径。
    ///
    /// # Errors
    /// 文件不存在或权限不足时返回底层 [`std::io::Error`]。
    pub(crate) fn open(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    /// 取文件总字节数。
    ///
    /// # Errors
    /// 元数据读取失败时返回 [`std::io::Error`]。
    pub(crate) fn len(&self) -> std::io::Result<u64> {
        let file = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(file.metadata()?.len())
    }
}

#[cfg_attr(all(feature = "mmap", not(feature = "wasm")), allow(dead_code))]
impl SegmentSource for FileSource {
    fn slice(&self) -> Option<&[u8]> {
        None
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<()> {
        // 锁中毒时恢复内部句柄继续工作(与 `memory/table/handle.rs` 的锁口径一致,不 panic)。
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        file.seek(SeekFrom::Start(off))?;
        file.read_exact(buf)
    }
}

/// 只读映射一个已打开的段文件(`memmap2::Mmap::map` 的唯一 `unsafe` 收敛点)。
///
/// 供 [`MmapSource`] 与存储后端 `FsStorage::open_bytes` 复用,避免 `unsafe`
/// 出现在本模块之外(AGENTS.md:库内 `unsafe` 仅允许 `src/core/simd/` 与本文件)。
/// 映射长度取映射时刻的文件长度;失败按 I/O 错误返回,绝不静默降级为整文件读入。
///
/// # Errors
/// mmap 失败时返回底层 [`std::io::Error`]。
#[cfg(all(feature = "mmap", not(feature = "wasm")))]
pub(crate) fn map_readonly(file: &File) -> std::io::Result<memmap2::Mmap> {
    // SAFETY: `Mmap::map` 要求映射期间文件不被截断/改写;段文件是 write-once 的,
    // 该前置由存储层逐条保证:
    // ① 所有写盘经 `storage::write_atomic`(临时文件 + fsync + rename)产生新
    //    inode,绝不原地覆盖或截断既有段文件;
    // ② 段文件一经 MANIFEST 引用即视为不可变,compaction 只把旧段移入 `trash/`
    //    (仅目录项变更,已打开的描述符/映射继续有效,POSIX unlink/rename 语义);
    // ③ 映射长度即映射时刻的 `File::metadata().len()`,映射期内不增长不改写。
    unsafe { memmap2::Mmap::map(file) }
}

/// 基于 `memmap2` 的零拷贝段数据源(feature `mmap`,默认开启)。
///
/// 整段只读映射由内核按页惰性载入,`slice()` 直接返回映射切片;文件不可变,
/// 故多线程共享安全。**关闭 feature `mmap`** 时用 [`FileSource`] 兜底(功能不变);
/// 开启后运行时 mmap 失败按 I/O 错误返回,不做静默降级。
#[cfg(all(feature = "mmap", not(feature = "wasm")))]
pub(crate) struct MmapSource {
    map: memmap2::Mmap,
}

#[cfg(all(feature = "mmap", not(feature = "wasm")))]
impl MmapSource {
    /// 只读 mmap 打开指定路径。
    ///
    /// # Errors
    /// 文件不存在、权限不足或 mmap 失败时返回底层 [`std::io::Error`]。
    pub(crate) fn open(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let map = map_readonly(&file)?;
        Ok(Self { map })
    }
}

#[cfg(all(feature = "mmap", not(feature = "wasm")))]
impl SegmentSource for MmapSource {
    fn slice(&self) -> Option<&[u8]> {
        Some(&self.map)
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<()> {
        // 32 位平台上 usize 装不下超过 4 GiB 的偏移;显式报错而非静默截断。
        let start = usize::try_from(off).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "偏移超出平台字长")
        })?;
        let end = start
            .checked_add(buf.len())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "偏移溢出"))?;
        if end > self.map.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "读取越界",
            ));
        }
        buf.copy_from_slice(&self.map[start..end]);
        Ok(())
    }
}

/// 经 [`SegmentSource`] 读取整段字节。
///
/// 开启 `mmap` 时用 [`MmapSource`] 的零拷贝切片(内核按页惰性载入)拷贝为 `Vec`;
/// 关闭时用 [`FileSource`](`Read + Seek`)。自段句柄惰性驻留落地后,生产打开路径
/// 改用 [`SegmentHandle`];`read_whole` 保留给测试与诊断路径(设计 04 §11)。
///
/// # Errors
/// 文件不存在或读取失败时返回底层 [`std::io::Error`]。
// reason: 生产打开已改走 `SegmentHandle`;`read_whole` 供测试与诊断(FC-PERSIST-POST-007)。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn read_whole(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    #[cfg(all(feature = "mmap", not(feature = "wasm")))]
    {
        let source = MmapSource::open(path)?;
        Ok(source.slice().map_or_else(Vec::new, <[u8]>::to_vec))
    }
    #[cfg(any(not(feature = "mmap"), feature = "wasm"))]
    {
        let source = FileSource::open(path)?;
        // 32 位平台上 usize 装不下超过 4 GiB 的长度;显式报错而非静默截断
        // (与 `MmapSource::read_at` 的偏移转换同口径)。
        let len = usize::try_from(source.len()?).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "文件长度超出平台字长")
        })?;
        let mut buf = vec![0_u8; len];
        source.read_at(0, &mut buf)?;
        Ok(buf)
    }
}
