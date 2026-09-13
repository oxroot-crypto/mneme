//! 向量索引抽象(`index.rs`):暴力扫描 → HNSW 的替换缝(L3,设计 03 §8 / 05 §12)。
//!
//! L1 只定义 trait 与数据载体,具体 HNSW 图实现见 `crate::index`;组合根
//! [`Builder`](crate::memory::Builder) 注入工厂(与 `persist` 同属已文档化的组合根例外)。
//! 除组合根外 `memory` 不依赖 `index` 的具体实现,层方向保持 L3 → L1,公开 API 不变。

use std::sync::Arc;

use crate::core::bitset::BitSet;
use crate::core::error::Result;
use crate::core::heap::TopK;
use crate::core::metric::{Metric, Score};
use crate::core::options::{HnswParams, VectorFormat};
use crate::core::types::{RowId, SlotId};

/// HNSW 每层邻居数硬上限。
///
/// 配置校验(`Builder::validate`)与 hidx 解码共用,保证"自产文件必可读回";
/// 同时防止恶意文件声明巨量度数导致内存膨胀。
pub(crate) const MAX_INDEX_DEGREE: u16 = 4096;

/// 建图节点的只读输入:稳定 `RowId`、向量 `Arc` 句柄与预计算范数平方。
#[derive(Debug, Clone)]
pub(crate) struct IndexNode {
    /// 稳定逻辑标识。
    pub(crate) rowid: RowId,
    /// 向量数据(零拷贝共享)。
    pub(crate) vector: Arc<[f32]>,
    /// 范数平方(余弦/欧氏复用)。
    pub(crate) norm_sq: f32,
}

/// 单个段的量化副本(i8/f16);f32 段为 `None`。
///
/// 副本与段同生同灭:flush/compaction 按当前配置生成,`open` 从 vsec qvec 区载入;
/// f32 原向量始终保留供精排(I12)。`rows` 与建图节点顺序一一对应。
#[derive(Debug, Clone)]
pub(crate) struct QuantCopy {
    /// 副本格式(`I8Rescored` 或 `F16`)。
    pub(crate) format: VectorFormat,
    /// i8 逐维 `(v_min, v_max)` 交错表;f16 为空。
    pub(crate) params: Vec<f32>,
    /// 每节点码流(节点顺序与 [`IndexNode`] 一致)。
    pub(crate) rows: Vec<Arc<[u8]>>,
}

impl QuantCopy {
    /// 单行码流字节数;`F32` 为 0。
    pub(crate) fn code_stride(&self) -> usize {
        let dimension = self.dimension();
        match self.format {
            VectorFormat::F32 => 0,
            VectorFormat::I8Rescored => dimension,
            VectorFormat::F16 => dimension * 2,
        }
    }

    /// 维度(i8 由参数表推得;f16 由首行码流推得;空副本为 0)。
    pub(crate) fn dimension(&self) -> usize {
        match self.format {
            VectorFormat::F32 => 0,
            VectorFormat::I8Rescored => self.params.len() / 2,
            VectorFormat::F16 => self.rows.first().map_or(0, |row| row.len() / 2),
        }
    }
}

/// 一次查询在某段上的量化形式(段级参数决定,每次索引搜索预计算一次)。
#[derive(Debug, Clone)]
pub(crate) enum QuantQuery {
    /// i8:查询侧权重与偏置。
    I8(crate::quant::scalar_i8::Query),
    /// f16:无预计算(以 f32 查询对 2 字节码流算点积)。
    #[cfg(feature = "quant-f16")]
    F16,
}

impl QuantQuery {
    /// 量化粗排分(点积为近似值,折算口径与 [`Metric::score`] 一致)。
    pub(crate) fn score(
        &self,
        metric: Metric,
        _query: &[f32],
        query_norm: f32,
        codes: &[u8],
        target_norm: f32,
    ) -> Score {
        let dot = match self {
            QuantQuery::I8(prepared) => prepared.score(codes),
            #[cfg(feature = "quant-f16")]
            QuantQuery::F16 => crate::quant::f16::coarse_dot_unchecked(_query, codes),
        };
        metric.score_from_dot(dot, query_norm, target_norm)
    }
}

/// 一次索引搜索的全部输入。
///
/// `alive` / `filter` 按**全局 `SlotId`** 索引;索引内部经 `slot_of` 映射到图节点。
/// 距离度量取自索引自身(建库即锁定),不在此重复传入以避免不一致。
pub(crate) struct IndexSearch<'a> {
    /// 查询向量。
    pub(crate) query: &'a [f32],
    /// 查询向量范数平方(度量需要时)。
    pub(crate) query_norm: f32,
    /// 探查宽度(已含三档放大前的用户值)。
    pub(crate) ef: usize,
    /// 返回条数。
    pub(crate) k: usize,
    /// 可见版本位图(全局槽位)。
    pub(crate) alive: &'a BitSet,
    /// 过滤候选位图(全局槽位;`None` = 无过滤)。
    pub(crate) filter: Option<&'a BitSet>,
    /// 过滤三档:后过滤 / 放大后过滤分界。
    pub(crate) post_threshold: f32,
    /// 过滤三档:放大后过滤 / 候选暴力分界。
    pub(crate) brute_threshold: f32,
    /// 是否使用段的量化副本做粗排(`false` = 精确 f32;召回抽样对照用)。
    pub(crate) use_quant: bool,
}

/// 单个段的向量索引与其覆盖的全局槽位(多段架构,设计 07 §4)。
///
/// 每个不可变段各持一张 HNSW 图;图节点 `n` 对应 `slots[n]` 个全局槽位。
/// 查询对全部段图分别搜索并归并 `TopK`;未被任何段索引覆盖的槽位(未落盘尾部或
/// 无 `hidx` 的段)仍走暴力扫描,二者统计等价。
#[derive(Clone)]
pub(crate) struct SegmentIndex {
    /// 所属段编号(统计与 compaction 用)。
    pub(crate) segment_id: u32,
    /// 该段的 HNSW 图(节点经内部 `slot_of` 映射到全局槽位)。
    pub(crate) index: Arc<dyn VectorIndex>,
    /// 图节点顺序的全局槽位(`slots[node] = slot_of(node)`)。
    pub(crate) slots: Vec<SlotId>,
    /// 覆盖位图(按全局槽位,O(1) 判定某槽位是否有索引)。
    pub(crate) covered: BitSet,
    /// 该段实际生效的量化格式(`F32` = 无副本或建段抽样已回退)。
    pub(crate) quant: VectorFormat,
    /// 建段抽样召回一致率估计(`None` = 无副本)。
    pub(crate) recall_est: Option<f32>,
}

impl SegmentIndex {
    /// 由段号、索引、覆盖槽位与量化元信息构造;`slots` 与图节点一一对应。
    pub(crate) fn new(
        segment_id: u32,
        index: Arc<dyn VectorIndex>,
        slots: Vec<SlotId>,
        quant: VectorFormat,
        recall_est: Option<f32>,
    ) -> Self {
        let mut covered = BitSet::default();
        for slot in &slots {
            covered.set(slot.get() as usize);
        }
        Self {
            segment_id,
            index,
            slots,
            covered,
            quant,
            recall_est,
        }
    }
}

/// 只读向量索引(对象安全)。
///
/// 节点 id 为段内下标;对外返回的 `SlotId` 由 [`VectorIndex`] 内部的 `slot_of`
/// 映射到全局槽位(新建索引为恒等映射,从 hidx 载入时按恢复重排映射)。
pub(crate) trait VectorIndex: Send + Sync {
    /// 图节点数(= 索引覆盖的全局槽位数)。
    fn node_count(&self) -> usize;

    /// 最高层(单节点图返回 0)。
    fn max_level(&self) -> u8;

    /// 段内入口点 `(全局槽位, 层级)`。
    fn entry(&self) -> (SlotId, u8);

    /// 编码为 hidx 字节(设计 05 §10)。
    ///
    /// # Errors
    /// 图规模超过 hidx 格式的 `u32` 长度上限时返回结构化错误(绝不静默截断)。
    fn serialize(&self) -> Result<Vec<u8>>;

    /// 在索引上搜索,返回按 `Metric::better` 排序的 top-k 载荷 `(RowId, SlotId)`。
    ///
    /// 分数不随载荷返回,由调用方在候选集内 O(1) 重算(与暴力路径一致)。
    fn search(&self, params: &IndexSearch<'_>) -> TopK<(RowId, SlotId)>;
}

/// 索引工厂:构建与载入(组合根注入)。
pub(crate) trait IndexFactory: Send + Sync {
    /// 由节点构建索引;`slot_of[node]` 为节点对应的全局槽位(多段增量段用)。
    ///
    /// `quant` 为段的量化副本(`None` = 纯 f32);图结构仍由 f32 向量构建,
    /// 副本只服务查询期粗排打分(设计 08 §4,落地取舍见 08 §落地状态)。
    fn build(
        &self,
        nodes: &[IndexNode],
        slot_of: &[SlotId],
        params: HnswParams,
        metric: Metric,
        quant: Option<QuantCopy>,
    ) -> Arc<dyn VectorIndex>;

    /// 校验 hidx 字节。
    ///
    /// # Errors
    /// 解析/CRC/版本失败时返回结构化错误。
    fn verify(&self, bytes: &[u8]) -> Result<()>;

    /// 由 hidx 字节载入索引。
    ///
    /// # Arguments
    /// * `nodes` - 段内节点顺序的输入(与 hidx 节点 id 对齐)。
    /// * `slot_of` - 段内节点 id → 全局槽位(恢复重排映射)。
    /// * `metric` - 库距离度量(建库即锁定,不存于 hidx)。
    /// * `quant` - 段的量化副本(`None` = 纯 f32)。
    ///
    /// 载入以 hidx 头部记录的图参数为准(打开时的 `HnswParams` 只影响新构建)。
    ///
    /// # Errors
    /// 魔数/版本/CRC/布局不符时返回结构化错误。
    fn load(
        &self,
        bytes: &[u8],
        nodes: &[IndexNode],
        slot_of: &[SlotId],
        metric: Metric,
        quant: Option<QuantCopy>,
    ) -> Result<Arc<dyn VectorIndex>>;
}
