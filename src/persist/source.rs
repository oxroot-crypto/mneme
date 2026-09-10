//! 段读取后端抽象(`source.rs`)。
//!
//! 索引层与恢复层只依赖 [`SegmentSource`];`mmap` 是**优化**而非功能依赖
//! (设计 04 §11)。按依赖白名单(01 §5),`memmap2`/`MmapSource` 自 L3 引入,
//! 本层只提供基于 `std`(`Read + Seek`)的 [`FileSource`]——功能完整,吞吐稍低。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Mutex;

/// 只读段数据源:支持整段切片(可选)与按偏移读取。
///
/// 实现者须保证 `Send + Sync` 且读取线程安全;`slice` 返回 `Some` 时调用方
/// 可直接零拷贝访问整段字节(`MmapSource`,L3),返回 `None` 时走 `read_at`。
pub(crate) trait SegmentSource: Send + Sync {
    /// 若后端支持零拷贝,返回整段字节切片;否则返回 `None`。
    fn slice(&self) -> Option<&[u8]>;

    /// 从偏移 `off` 起读满 `buf`。
    ///
    /// # Errors
    /// 底层文件读取失败或不足 `buf.len()` 字节时返回 [`std::io::Error`]。
    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::io::Result<()>;
}

/// 基于 `std::fs::File` 的段数据源(`seek + read`)。
///
/// 文件句柄经 [`Mutex`] 串行化,满足 `Sync`;段文件不可变,故无写竞争。
pub(crate) struct FileSource {
    file: Mutex<File>,
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// FileSource 按偏移读取与底层文件内容一致。
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
        assert!(source.read_at(6, &mut buf).is_err());
    }
}
