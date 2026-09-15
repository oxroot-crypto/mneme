//! `source` 模块的单元测试(后端读取路径与段句柄校验)。

use std::fs::File;
use std::io::Write;
use std::sync::Arc;

use crate::core::error::MnemeError;
use crate::memory::lazy::ByteSource;
use crate::persist::storage::{SEGMENTS_DIR, Storage, msec_name, vsec_name};

use super::byte_file::ByteFileOpenInput;
use super::*;

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

    let file = ByteFile::open(&ByteFileOpenInput {
        storage: &(Arc::new(crate::persist::storage::FsStorage::new(dir.path()))
            as Arc<dyn Storage>),
        segment_id: 7,
        scope: b"vsec",
        name: &vsec_name(7),
        encryption: None,
    })
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
    let missing = SegmentHandle::open(&SegmentHandleOpenInput {
        storage: &(Arc::new(crate::persist::storage::FsStorage::new(dir.path()))
            as Arc<dyn Storage>),
        segment_id: 1,
        expected_hidx_crc: 0,
        fail_fast: false,
        encryption: None,
    });
    assert!(matches!(
        missing,
        Err(MnemeError::Corrupted { segment: Some(id), .. }) if id.get() == 1
    ));
    std::fs::write(dir.path().join(SEGMENTS_DIR).join(vsec_name(1)), b"").expect("write");
    std::fs::write(dir.path().join(SEGMENTS_DIR).join(msec_name(1)), b"m").expect("write");
    let empty = SegmentHandle::open(&SegmentHandleOpenInput {
        storage: &(Arc::new(crate::persist::storage::FsStorage::new(dir.path()))
            as Arc<dyn Storage>),
        segment_id: 1,
        expected_hidx_crc: 0,
        fail_fast: false,
        encryption: None,
    });
    assert!(matches!(
        empty,
        Err(MnemeError::Corrupted { segment: Some(id), .. }) if id.get() == 1
    ));
}
