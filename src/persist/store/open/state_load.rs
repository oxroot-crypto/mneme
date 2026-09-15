//! 载入 MANIFEST 所列段并回放 WAL,重建写状态。

use std::sync::Arc;

use crate::core::error::Result;
use crate::memory::analysis::{BLOOM_INITIAL_CAPACITY, BloomSet, ZoneIndex};
use crate::memory::table::WriterState;
use crate::persist::manifest::Manifest;
use crate::persist::recover;
use crate::persist::store::manifest_io;
use crate::persist::store::wal_writer;

use super::hidx_load::{HidxxLoadInput, load_hidx_indexes};
use super::options::OpenOptions;

/// 载入 MANIFEST 所列段并回放 WAL,重建写状态。
///
/// 损坏段(头部/区级结构不可解析)在非 fail-fast 下仅内存跳过,文件原地保留
/// (MANIFEST 仍引用,移动会使后续打开拒启)。
pub(super) fn load_write_state(
    manifest: &Manifest,
    options: &OpenOptions,
    storage: &Arc<dyn crate::persist::storage::Storage>,
) -> Result<WriterState> {
    let mut state = recover::empty_state(manifest)?;
    prepare_rebuild_structures(&mut state, manifest, options);
    let segments = manifest_io::open_segment_handles(
        storage,
        manifest,
        options.fail_fast_on_corruption,
        options.encryption.as_ref(),
    )?;
    let recovered = recover::load_segments(
        &mut state,
        &segments,
        options.verify_on_open,
        options.fail_fast_on_corruption,
    )?;
    // 损坏段保持在原地、仅在内存跳过:MANIFEST 仍引用它们,绝不自动移到
    // `trash/`(否则下次打开会因"引用段缺失"拒绝启动,隔离变成删数据)。
    // 段数据可能仍可人工修复,`check()` 会报告损坏段。
    if let Some(factory) = options.index_factory.as_ref() {
        state.indexes = load_hidx_indexes(
            &state,
            &HidxxLoadInput {
                segments: &segments,
                recovered: &recovered,
                options,
                factory,
                metric: manifest.metric,
            },
        )?;
    }
    replay_all_wal(storage.as_ref(), &mut state, manifest, options)?;
    Ok(state)
}

/// 初始化恢复期重建加速结构所需的配置口径。
///
/// 必须与建库配置同口径(分词/字段上限/bloom 误判率):停用词开关以 MANIFEST 为准
/// (建库即锁定),否则查询分词与索引分词不一致会静默漏召回。
fn prepare_rebuild_structures(state: &mut WriterState, manifest: &Manifest, options: &OpenOptions) {
    state.stopwords_enabled = manifest.stopwords;
    state.index_fields_max = options.tuning.field_dict_max as usize;
    state.bloom_fpp = options.tuning.bloom_fpp;
    state.zones = Arc::new(ZoneIndex::new(options.tuning.field_dict_max as usize));
    state.key_bloom = Arc::new(BloomSet::new(
        BLOOM_INITIAL_CAPACITY,
        options.tuning.bloom_fpp,
    ));
}

/// 回放全部 WAL 文件(仅 seqno > watermark),并截断最后一个文件的撕裂尾部。
fn replay_all_wal(
    storage: &dyn crate::persist::storage::Storage,
    state: &mut WriterState,
    manifest: &Manifest,
    options: &OpenOptions,
) -> Result<()> {
    let wal_files = wal_writer::wal_files(storage)?;
    for (position, rel) in wal_files.iter().enumerate() {
        let bytes = storage.read_file(rel)?;
        let valid_len = recover::replay_wal(
            state,
            &bytes,
            manifest.watermark_seqno,
            options.encryption.as_ref(),
        )?;
        // 撕裂帧之后的字节会永久屏蔽后续追加,必须物理截断后再复用该 WAL;
        // 只有最后一个文件可能带撕裂尾(轮转前该文件已完整 fsync)。
        if position + 1 == wal_files.len() && !options.read_only && valid_len < bytes.len() {
            storage.truncate(rel, valid_len as u64)?;
        }
    }
    Ok(())
}
