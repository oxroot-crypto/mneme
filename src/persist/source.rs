//! 段读取后端抽象(`source.rs`)。
//!
//! 索引层与恢复层只依赖 [`SegmentSource`];`mmap` 是**优化**而非功能依赖
//! (设计 04 §11)。按依赖白名单(01 §5),`memmap2`/`MmapSource` 自 L3 引入:
//! feature `mmap`(默认开)经 [`MmapSource`] 零拷贝映射,关闭时回退本层基于
//! `std`(`Read + Seek`)的 [`FileSource`]——功能完整,吞吐稍低。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Mutex;

/// 只读段数据源:支持整段切片(可选)与按偏移读取。
///
/// 实现者须保证 `Send + Sync` 且读取线程安全;`slice` 返回 `Some` 时调用方
/// 可直接零拷贝访问整段字节(`MmapSource`,L3),返回 `None` 时走 `read_at`。
pub(crate) trait SegmentSource: Send + Sync {
    /// 若后端支持零拷贝,返回整段字节切片;否则返回 `None`。
    // reason: 关闭 `mmap` 时无零拷贝后端,`slice` 仅保留接口形状(设计 04 §11)。
    #[cfg_attr(not(feature = "mmap"), allow(dead_code))]
    fn slice(&self) -> Option<&[u8]>;

    /// 从偏移 `off` 起读满 `buf`。
    ///
    /// # Errors
    /// 底层文件读取失败或不足 `buf.len()` 字节时返回 [`std::io::Error`]。
    // reason: 启用 `mmap` 时生产路径走零拷贝 `slice`,该回退读法仅 `FileSource` 使用。
    #[cfg_attr(feature = "mmap", allow(dead_code))]
    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<()>;
}

/// 基于 `std::fs::File` 的段数据源(`seek + read`)。
///
/// 文件句柄经 [`Mutex`] 串行化,满足 `Sync`;段文件不可变,故无写竞争。
// reason: 启用 `mmap` 时生产路径走 `MmapSource`,`FileSource` 保留为显式回退后端
// (设计 04 §11);`--no-default-features`(关闭 feature `mmap`)下启用。
#[cfg_attr(feature = "mmap", allow(dead_code))]
pub(crate) struct FileSource {
    file: Mutex<File>,
}

#[cfg_attr(feature = "mmap", allow(dead_code))]
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

#[cfg_attr(feature = "mmap", allow(dead_code))]
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

/// 基于 `memmap2` 的零拷贝段数据源(feature `mmap`,默认开启)。
///
/// 整段只读映射由内核按页惰性载入,`slice()` 直接返回映射切片;文件不可变,
/// 故多线程共享安全。**关闭 feature `mmap`** 时用 [`FileSource`] 兜底(功能不变);
/// 开启后运行时 mmap 失败按 I/O 错误返回,不做静默降级。
#[cfg(feature = "mmap")]
pub(crate) struct MmapSource {
    map: memmap2::Mmap,
}

#[cfg(feature = "mmap")]
impl MmapSource {
    /// 只读 mmap 打开指定路径。
    ///
    /// # Errors
    /// 文件不存在、权限不足或 mmap 失败时返回底层 [`std::io::Error`]。
    pub(crate) fn open(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let file = File::open(path)?;
        // SAFETY: 段文件是 write-once 的——库内任何路径都不原地写入/截断已提交段
        // (单写者经临时文件 + rename 生成新段);`Mmap::map` 要求映射期间文件不被
        // 截断/改写,该不变量由存储层保证。映射长度取映射时刻的文件长度。
        let map = unsafe { memmap2::Mmap::map(&file)? };
        Ok(Self { map })
    }
}

#[cfg(feature = "mmap")]
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
/// 关闭时用 [`FileSource`](`Read + Seek`)。L3 恢复阶段需要自有字节以重建内存表,
/// 真正的"零拷贝驻留"随 L6 的段句柄重构落地(设计 04 §11)。
///
/// # Errors
/// 文件不存在或读取失败时返回底层 [`std::io::Error`]。
pub(crate) fn read_whole(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    #[cfg(feature = "mmap")]
    {
        let source = MmapSource::open(path)?;
        Ok(source.slice().map_or_else(Vec::new, <[u8]>::to_vec))
    }
    #[cfg(not(feature = "mmap"))]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// FileSource 按偏移读取与底层文件内容一致,越界返回 `UnexpectedEof`。
    #[test]
    fn file_source_reads_at_offset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seg.bin");
        let mut file = File::create(&path).expect("create");
        file.write_all(&[1, 2, 3, 4, 5, 6, 7, 8]).expect("write");
        drop(file);

        let source = FileSource::open(&path).expect("open");
        assert!(source.slice().is_none());
        assert_eq!(source.len().expect("len"), 8);
        let mut buf = [0_u8; 4];
        source.read_at(3, &mut buf).expect("read_at");
        assert_eq!(buf, [4, 5, 6, 7]);
        assert_eq!(
            source.read_at(6, &mut buf).expect_err("越界应报错").kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    /// `read_whole` 读取整段字节:开启 `mmap` 时经映射,关闭时经 `FileSource`。
    #[test]
    fn read_whole_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seg.bin");
        std::fs::write(&path, b"hidx-bytes").expect("write");
        assert_eq!(super::read_whole(&path).expect("read"), b"hidx-bytes");
    }

    /// **FC-PERSIST-POST-007**:`read_whole` 与 `std::fs::read` 逐字节一致
    /// (feature `mmap` 开/关两条实现路径产出相同结果)。
    #[test]
    fn read_whole_matches_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seg.bin");
        let payload: Vec<u8> = (0..=255_u8).cycle().take(4096).collect();
        std::fs::write(&path, &payload).expect("write");
        assert_eq!(super::read_whole(&path).expect("read_whole"), payload);
    }

    /// **FC-PERSIST-POST-007**:`MmapSource` 整段切片与文件一致,越界读取返回
    /// `UnexpectedEof`(而非静默短读)。
    #[cfg(feature = "mmap")]
    #[test]
    fn mmap_source_slice_and_bounds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seg.bin");
        std::fs::write(&path, b"abcdefgh").expect("write");
        let source = MmapSource::open(&path).expect("open");
        assert_eq!(source.slice(), Some(&b"abcdefgh"[..]));
        let mut buf = [0_u8; 4];
        source.read_at(2, &mut buf).expect("read_at");
        assert_eq!(buf, *b"cdef");
        assert_eq!(
            source.read_at(6, &mut buf).expect_err("越界应报错").kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
}
