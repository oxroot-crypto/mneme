//! `storage` 模块的单元测试(原子写入、路径安全、文件锁与 MANIFEST 命名)。

use crate::core::error::MnemeError;

use super::io::tmp_path;
use super::*;

/// 原子写入后可读回,`.tmp` 不残留。
#[test]
fn write_atomic_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_atomic(dir.path(), "current", b"42").expect("write");
    assert_eq!(read_file(dir.path(), "current").expect("read"), b"42");
    assert!(!exists(&tmp_path(&dir.path().join("current"))).expect("exists"));
}

/// `write_new` 不覆盖既有文件。
#[test]
fn write_new_does_not_overwrite() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_new(dir.path(), "a", b"1").expect("first");
    assert!(matches!(
        write_new(dir.path(), "a", b"2"),
        Err(crate::core::error::MnemeError::Io(_))
    ));
    assert_eq!(std::fs::read(dir.path().join("a")).expect("read"), b"1");
}

/// 路径穿越被拒绝。
#[test]
fn resolve_rejects_traversal() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(resolve(dir.path(), "../evil").is_err());
    assert!(resolve(dir.path(), "/abs").is_err());
    assert!(resolve(dir.path(), "wal/wal_000001.log").is_ok());
}

/// 锁互斥与释放后重取。
#[test]
fn file_lock_blocks_second_holder() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = FileLock::acquire(dir.path()).expect("first");
    assert!(matches!(
        FileLock::acquire(dir.path()),
        Err(MnemeError::Busy(_))
    ));
    drop(first);
    assert!(FileLock::acquire(dir.path()).is_ok());
}

/// 锁文件已存在但无 OS 持有者(如崩溃残留)时不阻塞——OS 咨询锁已随进程释放。
#[test]
fn file_lock_acquires_when_lock_file_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(LOCK_FILE), b"999\n0\n").expect("write stale lock");
    assert!(FileLock::acquire(dir.path()).is_ok());
}

/// `Drop` 释放锁但不删除锁文件,避免不同 inode 各自加锁破坏互斥。
#[test]
fn file_lock_file_persists_after_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lock = FileLock::acquire(dir.path()).expect("acquire");
    let path = dir.path().join(LOCK_FILE);
    assert!(path.exists());
    drop(lock);
    assert!(path.exists(), "锁文件应保留");
    assert!(FileLock::acquire(dir.path()).is_ok(), "释放后可重新获取");
}

/// MANIFEST 文件名解析。
#[test]
fn manifest_name_roundtrip() {
    assert_eq!(manifest_name(42), "MANIFEST.000042");
    assert_eq!(parse_manifest_name("MANIFEST.000042"), Some(42));
    assert_eq!(parse_manifest_name("current"), None);
}
