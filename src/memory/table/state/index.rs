//! 检索加速结构(倒排 / zone map / bloom)维护与段索引安装。

use std::sync::Arc;

use crate::core::options::VectorFormat;
use crate::core::types::SlotId;
use crate::memory::analysis::{BLOOM_INITIAL_CAPACITY, BloomSet, InvertedIndex, ZoneIndex};
use crate::memory::index::{SegmentIndex, SegmentIndexInput, VectorIndex};

use super::WriterState;
use super::slot::SlotData;

/// [`WriterState::install_segment`] 的输入参数。
pub(crate) struct InstallSegmentInput<'a> {
    /// 段编号(MANIFEST 中已提交)。
    pub(crate) segment_id: u32,
    /// 该段包含的全局槽位(升序)。
    pub(crate) slot_indices: &'a [usize],
    /// 该段索引(`None` = 无 hidx 或未配置索引工厂,其槽位由暴力扫描覆盖)。
    pub(crate) index: Option<Arc<dyn VectorIndex>>,
    /// 该段实际生效的量化格式。
    pub(crate) quant: VectorFormat,
    /// 建段抽样召回估计(`None` = 无副本)。
    pub(crate) recall_est: Option<f32>,
}

impl WriterState {
    /// 把新提交的记录增量加入检索加速结构(倒排 / zone map / key bloom)。
    ///
    /// 墓碑不参与(zone 只统计实际字段值);被遮蔽的旧版本保留在索引中,
    /// 由查询期按视图可见性过滤,`as_of` 历史视图因此仍可检索旧版本文本。
    pub(super) fn index_observe(&mut self, slot: SlotId, slot_data: &SlotData) {
        if let Some(text) = &slot_data.text {
            Arc::make_mut(&mut self.inv).insert_text(
                slot,
                slot_data.ns_id,
                text,
                self.stopwords_enabled,
            );
        }
        Arc::make_mut(&mut self.zones).observe(slot.get() as usize, slot_data);
        if let Some(key) = &slot_data.key {
            Arc::make_mut(&mut self.key_bloom).insert(key.as_str());
        }
    }

    /// 从槽位全量重建三类加速结构(无磁盘索引或映射不可用时使用)。
    ///
    /// 分词开关与字段上限取自本状态(建库/打开时由配置注入)。
    pub(crate) fn rebuild_indexes(&mut self) {
        self.inv = Arc::new(InvertedIndex::default());
        self.zones = Arc::new(ZoneIndex::new(self.index_fields_max));
        self.key_bloom = Arc::new(BloomSet::new(BLOOM_INITIAL_CAPACITY, self.bloom_fpp));
        for index in 0..self.slots.len() {
            let slot_data = Arc::clone(&self.slots[index]);
            if slot_data.deleted {
                continue;
            }
            // 槽位下标 ≤ u32::MAX(FC-MEM-INV-004),转换可证明不会失败。
            let slot =
                SlotId::new(u32::try_from(index).expect("槽位下标必可转入 u32(FC-MEM-INV-004)"));
            self.index_observe(slot, &slot_data);
        }
    }

    /// 装载磁盘倒排与 bloom,并从槽位重建 zone map。
    ///
    /// zone map 无法直接复用段内块统计:恢复按 `(rowid, seqno)` 重排槽位后,
    /// 段内块与全局块不再对应;重建结果与磁盘内容等价(roundtrip 测试保证)。
    pub(crate) fn load_disk_indexes(&mut self, inv: InvertedIndex, bloom: BloomSet) {
        self.inv = Arc::new(inv);
        self.key_bloom = Arc::new(bloom);
        self.zones = Arc::new(ZoneIndex::new(self.index_fields_max));
        for index in 0..self.slots.len() {
            if self.slots[index].deleted {
                continue;
            }
            Arc::make_mut(&mut self.zones).observe(index, &self.slots[index]);
        }
    }

    /// 装载多段合并倒排并按需重建 bloom / zone map(多段恢复路径)。
    ///
    /// `bloom = None` 时从全部槽位重建(各段 bloom 参数不一致时无法按位或合并);
    /// zone map 逐段块偏移与全局块不再对应,统一从槽位重建。
    pub(crate) fn load_merged_indexes(&mut self, inv: InvertedIndex, bloom: Option<BloomSet>) {
        self.inv = Arc::new(inv);
        self.zones = Arc::new(ZoneIndex::new(self.index_fields_max));
        self.key_bloom = Arc::new(BloomSet::new(BLOOM_INITIAL_CAPACITY, self.bloom_fpp));
        for index in 0..self.slots.len() {
            let slot_data = Arc::clone(&self.slots[index]);
            if slot_data.deleted {
                continue;
            }
            Arc::make_mut(&mut self.zones).observe(index, &slot_data);
            if let Some(key) = &slot_data.key {
                Arc::make_mut(&mut self.key_bloom).insert(key.as_str());
            }
        }
        if let Some(bloom) = bloom {
            self.key_bloom = Arc::new(bloom);
        }
    }

    /// 安装刚提交的段:登记槽位归属与段索引(供查询多图归并)。
    ///
    /// `slot_indices` 为该段包含的全局槽位(升序);`index = None` 表示该段无
    /// `hidx`(或未配置索引工厂),其槽位由查询期暴力覆盖。
    /// `quant`/`recall_est` 为该段实际生效的量化格式与建段抽样召回估计。
    pub(crate) fn install_segment(&mut self, input: InstallSegmentInput<'_>) {
        let InstallSegmentInput {
            segment_id,
            slot_indices,
            index,
            quant,
            recall_est,
        } = input;
        for &idx in slot_indices {
            if let Some(entry) = self.slot_segment.get_mut(idx) {
                *entry = Some(segment_id);
            }
        }
        if let Some(index) = index {
            let slots: Vec<SlotId> = slot_indices
                .iter()
                .map(|&idx| {
                    // 槽位下标 ≤ u32::MAX(FC-MEM-INV-004),转换可证明不会失败。
                    SlotId::new(u32::try_from(idx).expect("槽位下标必可转入 u32(FC-MEM-INV-004)"))
                })
                .collect();
            Arc::make_mut(&mut self.indexes).push(SegmentIndex::new(SegmentIndexInput {
                segment_id,
                index,
                slots,
                quant,
                recall_est,
            }));
        }
    }
}
