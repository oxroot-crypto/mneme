//! 段读取后端抽象(`source.rs`)。
//!
//! 索引层与恢复层只依赖 [`SegmentSource`];`mmap` 是**优化**而非功能依赖
//! (设计 04 §11)。按依赖白名单(01 §5),`memmap2`/`MmapSource` 自 L3 引入:
//! feature `mmap`(默认开)经 [`MmapSource`] 零拷贝映射,关闭时回退本层基于
//! `std`(`Read + Seek`)的 [`FileSource`]——功能完整,吞吐稍低。
//!
//! 自段句柄惰性驻留(FC-PERSIST-INV-021)起,打开路径用 [`SegmentHandle`]
//! 长期持有段文件:[`ByteFile`] 实现 L1 [`ByteSource`](crate::memory::lazy::ByteSource),
//! 向量/量化码/图邻接按需切片;非 mmap 构建整文件读入自有缓冲,功能等价。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::Mutex;

use crate::core::error::{MnemeError, Result};
use crate::core::types::SegmentId;
use crate::memory::lazy::ByteSource;
use crate::persist::storage::{SEGMENTS_DIR, Storage, hidx_name, msec_name, vsec_name};

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
/// 出现在本模块之外(AGENTS.md:库内 `unsafe` 仅允许 `src/core/simd.rs` 与本文件)。
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

impl ByteFile {
    /// 只读打开 `segments/<name>`。
    ///
    /// # Errors
    /// 文件缺失/权限不足/映射失败时返回底层 [`std::io::Error`]。
    pub(crate) fn open(
        storage: &Arc<dyn Storage>,
        segment_id: u32,
        scope: &[u8],
        name: &str,
        encryption: Option<&crate::crypto::Encryption>,
    ) -> Result<Arc<Self>> {
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

/// 一个段的三文件句柄集(vsec/msec/可选 hidx)。
///
/// 持 [`Arc<ByteFile>`] 即保证对应文件在句柄存活期内可读(惰性向量与惰性图
/// 各自持有 vsec/hidx 句柄);compaction 移动旧段只影响目录项,不影响已打开的
/// 描述符/映射(FC-PERSIST-INV-021)。
#[derive(Debug)]
pub(crate) struct SegmentHandle {
    /// 段编号。
    pub(crate) segment_id: u32,
    /// 向量段文件。
    pub(crate) vsec: Arc<ByteFile>,
    /// 元数据段文件。
    pub(crate) msec: Arc<ByteFile>,
    /// HNSW 图文件(MANIFEST `hidx_crc == 0` 时为 `None`)。
    pub(crate) hidx: Option<Arc<ByteFile>>,
}

impl SegmentHandle {
    /// 打开一个段的三文件句柄,校验存在性/非空与 hidx 整文件 CRC。
    ///
    /// vsec/msec 缺失或为空 → [`MnemeError::Corrupted`](I2/I3);hidx 缺失/为空/
    /// CRC 不符时:`fail_fast` 报 `Corrupted`,否则返回 `None`(降级暴力,与
    /// 设计 05 §12「索引是优化」一致);`expected_hidx_crc == 0` 表示无索引。
    ///
    /// # Errors
    /// 见上;其余 I/O 失败返回 [`MnemeError::Io`]。
    pub(crate) fn open(
        storage: &Arc<dyn Storage>,
        segment_id: u32,
        expected_hidx_crc: u32,
        fail_fast: bool,
        encryption: Option<&crate::crypto::Encryption>,
    ) -> Result<Self> {
        let vsec = open_required(
            storage,
            segment_id,
            b"vsec",
            &vsec_name(segment_id),
            encryption,
        )?;
        let msec = open_required(
            storage,
            segment_id,
            b"msec",
            &msec_name(segment_id),
            encryption,
        )?;
        let hidx = open_optional(
            storage,
            segment_id,
            expected_hidx_crc,
            fail_fast,
            encryption,
        )?;
        Ok(Self {
            segment_id,
            vsec,
            msec,
            hidx,
        })
    }
}

/// 打开被 MANIFEST 引用的段文件;不存在或为空 → `Corrupted`。
fn open_required(
    storage: &Arc<dyn Storage>,
    segment_id: u32,
    scope: &[u8],
    name: &str,
    encryption: Option<&crate::crypto::Encryption>,
) -> Result<Arc<ByteFile>> {
    match ByteFile::open(storage, segment_id, scope, name, encryption) {
        Ok(file) if file.len() > 0 => Ok(file),
        Ok(_) => Err(MnemeError::Corrupted {
            segment: Some(SegmentId::new(segment_id)),
            reason: format!("MANIFEST 引用的段文件为空:{name}"),
        }),
        Err(MnemeError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(MnemeError::Corrupted {
                segment: Some(SegmentId::new(segment_id)),
                reason: format!("MANIFEST 引用的段文件缺失:{name}"),
            })
        }
        Err(error) => Err(error),
    }
}

/// 打开可选 hidx 文件并核对整文件 CRC;失败按 `fail_fast` 决定上报或降级。
fn open_optional(
    storage: &Arc<dyn Storage>,
    segment_id: u32,
    expected_crc: u32,
    fail_fast: bool,
    encryption: Option<&crate::crypto::Encryption>,
) -> Result<Option<Arc<ByteFile>>> {
    if expected_crc == 0 {
        return Ok(None);
    }
    let name = hidx_name(segment_id);
    let file = match ByteFile::open(storage, segment_id, b"hidx", &name, encryption) {
        Ok(file) => file,
        Err(MnemeError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return if fail_fast {
                Err(MnemeError::Corrupted {
                    segment: Some(SegmentId::new(segment_id)),
                    reason: format!("MANIFEST 引用的 hidx 缺失:{name}"),
                })
            } else {
                Ok(None)
            };
        }
        Err(error) => return Err(error),
    };
    let crc_ok = file
        .slice_at(0, file.len())
        .is_some_and(|bytes| crate::persist::crc32(bytes) == expected_crc);
    if !crc_ok {
        return if fail_fast {
            Err(MnemeError::Corrupted {
                segment: Some(SegmentId::new(segment_id)),
                reason: format!("hidx 文件 CRC 与 MANIFEST 不符:{name}"),
            })
        } else {
            Ok(None)
        };
    }
    Ok(Some(file))
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
    #[cfg(all(feature = "mmap", not(feature = "wasm")))]
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

    /// **FC-PERSIST-INV-021**:`ByteFile` 整段切片与文件字节逐字节一致,
    /// 区间越界返回 `None`(惰性读取的边界口径)。
    #[test]
    fn byte_file_slice_matches_file_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let payload: Vec<u8> = (0..=255_u8).cycle().take(1024).collect();
        let path = dir.path().join(SEGMENTS_DIR).join(vsec_name(7));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, &payload).expect("write");

        let file = ByteFile::open(
            &(Arc::new(crate::persist::storage::FsStorage::new(dir.path())) as Arc<dyn Storage>),
            7,
            b"vsec",
            &vsec_name(7),
            None,
        )
        .expect("open");
        assert_eq!(file.len(), payload.len());
        assert_eq!(file.byte_len(), payload.len());
        assert_eq!(file.segment_id(), 7);
        assert_eq!(file.slice_at(0, payload.len()), Some(payload.as_slice()));
        assert_eq!(file.slice_at(10, 4), Some(&payload[10..14]));
        assert_eq!(file.slice_at(payload.len() - 2, 4), None, "越界必须拒绝");
        assert_eq!(file.slice_at(0, payload.len() + 1), None);
    }

    /// **FC-PERSIST-INV-021**:`SegmentHandle` 打开校验段文件存在性/非空
    /// (缺失/为空 → `Corrupted`,绝不静默跳过)。
    #[test]
    fn segment_handle_rejects_missing_or_empty_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(SEGMENTS_DIR)).expect("mkdir");
        let missing = SegmentHandle::open(
            &(Arc::new(crate::persist::storage::FsStorage::new(dir.path())) as Arc<dyn Storage>),
            1,
            0,
            false,
            None,
        );
        assert!(matches!(
            missing,
            Err(MnemeError::Corrupted { segment: Some(id), .. }) if id.get() == 1
        ));
        std::fs::write(dir.path().join(SEGMENTS_DIR).join(vsec_name(1)), b"").expect("write");
        std::fs::write(dir.path().join(SEGMENTS_DIR).join(msec_name(1)), b"m").expect("write");
        let empty = SegmentHandle::open(
            &(Arc::new(crate::persist::storage::FsStorage::new(dir.path())) as Arc<dyn Storage>),
            1,
            0,
            false,
            None,
        );
        assert!(matches!(
            empty,
            Err(MnemeError::Corrupted { segment: Some(id), .. }) if id.get() == 1
        ));
    }
}
