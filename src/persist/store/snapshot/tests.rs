//! `snapshot` 增量 flush 同备份个单元测试。

use super::*;

/// 切块覆盖全部槽位且保序、不重叠;空槽位(纯 delta 段)仍产生一个空块;
/// 块行数尊重 `Tuning.flush_chunk_rows` 配置。
#[test]
fn split_slot_chunks_covers_all_slots_in_order() {
    let tuning = crate::core::options::Tuning::default();
    let slots: Vec<usize> = (0..10_000).collect();
    let chunks = split_slot_chunks(&slots, &tuning);
    assert!(!chunks.is_empty());
    let flat: Vec<usize> = chunks
        .iter()
        .flat_map(|chunk| chunk.iter().copied())
        .collect();
    assert_eq!(flat, slots, "切块必须按序覆盖全部槽位且不重叠/不遗漏");

    let empty = split_slot_chunks(&[], &tuning);
    assert_eq!(empty.len(), 1, "空槽位需要一个空块与纯 delta 段一一对应");
    assert!(empty[0].is_empty());

    let small_chunks = {
        let custom = crate::core::options::Tuning {
            flush_chunk_rows: 1_024,
            ..tuning
        };
        split_slot_chunks(&slots, &custom)
    };
    assert_eq!(small_chunks.len(), 10, "块行数配置应生效");
}

/// 并行块数受内存预算约束:1536 维 65,536 行块单块约 0.82GB,6GB 预算下
/// 至多 7 块并行;小维度(预算充足)不被额外收紧。
#[test]
fn flush_build_threads_bounded_by_memory_budget() {
    // 1536 维 × 65,536 行:约 0.82GB/块 → 6GB / 0.82GB = 7。
    assert_eq!(flush_build_threads(1536, 65_536, 8, 16), 7);
    // 16 核也不超过预算允许的并发。
    assert_eq!(flush_build_threads(1536, 65_536, 16, 16), 7);
    // 小维度:预算充足时只受并行度与块数约束。
    assert_eq!(flush_build_threads(128, 65_536, 8, 16), 8);
    assert_eq!(flush_build_threads(128, 65_536, 8, 3), 3);
    // 退化输入:至少 1 块。
    assert_eq!(flush_build_threads(0, 0, 0, 0), 1);
}

/// FC-PERSIST-STA-004:块数由行数决定、**每块不超过 `chunk_rows`**;并行度只限
/// 同时构建的块数,不得放大单块规模(修复「并行度越大块越大」的行为)。
#[test]
fn split_slot_chunks_limits_block_rows() {
    let slots: Vec<usize> = (0..10_000).collect();
    let chunks = split_slot_chunks_with(&slots, 1_024);
    assert_eq!(chunks.len(), 10, "10_000 行按 1_024 行应切 10 块");
    assert!(
        chunks.iter().all(|chunk| chunk.len() <= 1_024),
        "每块不得超过块行数上限"
    );
    assert_eq!(
        chunks.iter().map(|chunk| chunk.len()).sum::<usize>(),
        10_000
    );
}

/// FC-LIFE-POST-008:硬链接失败(目标已存在/跨盘)时回退逐文件复制,
/// `hardlinked` 如实置 `false`,产物内容正确。
#[test]
fn hardlink_failure_falls_back_to_copy() {
    let root = tempfile::tempdir().expect("root");
    let target = tempfile::tempdir().expect("target");
    let rel = format!("{SEGMENTS_DIR}/seg_000001.vsec");
    let source = storage::resolve(root.path(), &rel).expect("source path");
    storage::ensure_dir(source.parent().expect("parent")).expect("segments dir");
    let target_dir = storage::resolve(target.path(), &rel).expect("target path");
    storage::ensure_dir(target_dir.parent().expect("parent")).expect("target dir");
    let content = b"fake segment bytes";
    std::fs::write(&source, content).expect("write source");
    // 目标已存在同名文件 → `hard_link` 失败 → 走复制回退。
    std::fs::write(&target_dir, b"stale").expect("write stale");

    let mut sink = CopySink {
        root: root.path(),
        target: target.path(),
        counts: CopyCounts::default(),
        hardlinked: true,
    };
    sink.link_or_copy_required(&rel).expect("copy fallback");
    assert!(!sink.hardlinked, "硬链接失败必须如实报告回退");
    assert_eq!(sink.counts.files, 1);
    assert_eq!(sink.counts.bytes, content.len() as u64);
    assert_eq!(std::fs::read(&target_dir).expect("read"), content);
}
