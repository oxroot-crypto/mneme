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
use crate::memory::lazy::{LazyRows, VectorStorage};

/// HNSW 每层邻居数硬上限。
///
/// 配置校验(`Builder::validate`)与 hidx 解码共用,保证"自产文件必可读回";
/// 同时防止恶意文件声明巨量度数导致内存膨胀。
pub(crate) const MAX_INDEX_DEGREE: u16 = 4096;

/// 建图节点的只读输入:稳定 `RowId`、向量句柄与预计算范数平方。
#[derive(Debug, Clone)]
pub(crate) struct IndexNode {
    /// 稳定逻辑标识。
    pub(crate) rowid: RowId,
    /// 向量数据(自有或段内惰性,`Arc` 共享缓存)。
    pub(crate) vector: Arc<VectorStorage>,
    /// 范数平方(余弦/欧氏复用)。
    pub(crate) norm_sq: f32,
}

/// 单个段的量化副本(i8/f16);f32 段为 `None`。
///
/// 副本与段同生同灭:flush/compaction 按当前配置生成,`open` 从 vsec qvec 区
/// 以惰性行区载入;f32 原向量始终保留供精排(I12)。行顺序与建图节点一一对应。
#[derive(Debug, Clone)]
pub(crate) struct QuantCopy {
    /// 副本格式(`I8Rescored` 或 `F16`)。
    pub(crate) format: VectorFormat,
    /// i8 逐维 `(v_min, v_max)` 交错表;f16 为空。
    pub(crate) params: Vec<f32>,
    /// 每节点码流(节点顺序与 [`IndexNode`] 一致;段内惰性行区)。
    pub(crate) rows: LazyRows,
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

    /// 维度(i8 由参数表推得;f16 由行距推得;空副本为 0)。
    pub(crate) fn dimension(&self) -> usize {
        match self.format {
            VectorFormat::F32 => 0,
            VectorFormat::I8Rescored => self.params.len() / 2,
            VectorFormat::F16 => self.rows.stride() / 2,
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

/// [`QuantQuery::score`] 的输入参数。
#[derive(Debug)]
pub(crate) struct QuantQueryScoreInput<'a> {
    /// 距离度量。
    pub(crate) metric: Metric,
    /// 查询向量(`F16` 粗排直接对码流算点积;非 `quant-f16` 构建不使用)。
    #[cfg_attr(not(feature = "quant-f16"), allow(dead_code))]
    pub(crate) query: &'a [f32],
    /// 查询向量范数平方。
    pub(crate) query_norm: f32,
    /// 节点码流。
    pub(crate) codes: &'a [u8],
    /// 目标向量范数平方。
    pub(crate) target_norm: f32,
}

impl QuantQuery {
    /// 量化粗排分(点积为近似值,折算口径与 [`Metric::score`] 一致)。
    pub(crate) fn score(&self, input: &QuantQueryScoreInput<'_>) -> Score {
        let dot = match self {
            QuantQuery::I8(prepared) => prepared.score(input.codes),
            #[cfg(feature = "quant-f16")]
            QuantQuery::F16 => crate::quant::f16::coarse_dot_unchecked(input.query, input.codes),
        };
        input
            .metric
            .score_from_dot(dot, input.query_norm, input.target_norm)
    }
}

/// 遍历期节点偏置(只改 HNSW 前沿出堆顺序,不改最终打分;FC-SCORE-POST-007)。
pub(crate) trait NodeBias: Send + Sync {
    /// 返回全局槽位的偏置值(0 = 无偏置);槽位不可见/不存在返回 0。
    fn bias(&self, slot: SlotId) -> f32;
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
    /// 重要性偏置(只改遍历顺序;`None` = 关闭,`Scoring::bias_routing`)。
    pub(crate) bias: Option<&'a dyn NodeBias>,
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

/// [`SegmentIndex::new`] 的输入参数。
pub(crate) struct SegmentIndexInput {
    /// 所属段编号。
    pub(crate) segment_id: u32,
    /// 该段的 HNSW 图(节点经内部 `slot_of` 映射到全局槽位)。
    pub(crate) index: Arc<dyn VectorIndex>,
    /// 图节点顺序的全局槽位(`slots[node] = slot_of(node)`)。
    pub(crate) slots: Vec<SlotId>,
    /// 该段实际生效的量化格式。
    pub(crate) quant: VectorFormat,
    /// 建段抽样召回一致率估计(`None` = 无副本)。
    pub(crate) recall_est: Option<f32>,
}

impl SegmentIndex {
    /// 由段号、索引、覆盖槽位与量化元信息构造;`slots` 与图节点一一对应。
    pub(crate) fn new(input: SegmentIndexInput) -> Self {
        let SegmentIndexInput {
            segment_id,
            index,
            slots,
            quant,
            recall_est,
        } = input;
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

/// 索引构建请求(参数收敛;字段语义见各字段文档)。
pub(crate) struct IndexBuildRequest<'a> {
    /// 建图节点(节点 id = 下标)。
    pub(crate) nodes: &'a [IndexNode],
    /// 节点 id → 全局槽位映射(与 `nodes` 等长;增量段用)。
    pub(crate) slot_of: &'a [SlotId],
    /// HNSW 图参数。
    pub(crate) params: HnswParams,
    /// 距离度量(建库即锁定)。
    pub(crate) metric: Metric,
    /// 段量化副本(`None` = 纯 f32),只服务查询期粗排打分。
    pub(crate) quant: Option<QuantCopy>,
    /// 建图距离精度档位(设计 05 §4.4、`FC-INDEX-POST-010`)。
    pub(crate) build_precision: crate::core::options::BuildPrecision,
    /// 建图工程参数(并行度/批行数/选邻比较上限,由 `Tuning` 派生;`FC-INDEX-POST-012`)。
    pub(crate) build: crate::core::options::HnswBuildParams,
}

/// [`IndexFactory::load`] 的输入参数。
#[derive(Debug)]
pub(crate) struct IndexLoadRequest<'a> {
    /// hidx 文件视图(只读头部与 `node_table`,邻接按需解码)。
    pub(crate) span: &'a crate::memory::lazy::ByteSpan,
    /// 段内节点顺序的输入(与 hidx 节点 id 对齐)。
    pub(crate) nodes: &'a [IndexNode],
    /// 段内节点 id → 全局槽位(恢复重排映射)。
    pub(crate) slot_of: &'a [SlotId],
    /// 库距离度量(建库即锁定,不存于 hidx)。
    pub(crate) metric: Metric,
    /// 段的量化副本(`None` = 纯 f32)。
    pub(crate) quant: Option<QuantCopy>,
}

/// 索引工厂:构建与载入(组合根注入)。
pub(crate) trait IndexFactory: Send + Sync {
    /// 由构建请求构建索引。
    ///
    /// # Errors
    /// 建图输入不满足档位前置(如向量维度不一致/非有限值导致段内量化失败)时
    /// 返回结构化错误,绝不静默降级为其它档位。
    fn build(&self, request: IndexBuildRequest<'_>) -> Result<Arc<dyn VectorIndex>>;

    /// 校验 hidx 字节。
    ///
    /// # Errors
    /// 解析/CRC/版本失败时返回结构化错误。
    fn verify(&self, bytes: &[u8]) -> Result<()>;

    /// 由 hidx 句柄载入索引。
    ///
    /// 载入以 hidx 头部记录的图参数为准(打开时的 `HnswParams` 只影响新构建);
    /// `request` 各字段语义见 [`IndexLoadRequest`]。
    ///
    /// # Errors
    /// 魔数/版本/CRC/布局不符时返回结构化错误。
    fn load(&self, request: IndexLoadRequest<'_>) -> Result<Arc<dyn VectorIndex>>;
}
