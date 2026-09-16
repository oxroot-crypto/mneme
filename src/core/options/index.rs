//! 检索与索引参数。
//!
//! 覆盖 HNSW 图参数(实现见 L3)、进阶调参与向量量化格式(实现见 L6);
//! 默认值与 Builder 方法见设计 16 §2。

/// HNSW 图参数(L3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswParams {
    /// 上层度数上限,默认 16。
    pub m: u16,
    /// 第 0 层度数上限,默认 32。
    pub m0: u16,
    /// 构建期探查宽度,默认 200。
    pub ef_construction: u16,
    /// 查询期默认探查宽度,默认 64。
    pub ef_search: u16,
}

impl Default for HnswParams {
    fn default() -> Self {
        Self {
            m: 16,
            m0: 32,
            ef_construction: 200,
            ef_search: 64,
        }
    }
}

/// 两阶段粗排过采样倍率缺省值:4 × `top_k`(设计 08 §4.2「放宽取 4k 个候选」)。
///
/// 定义在 core 侧以免 L0 反向依赖 L6(`src/quant/`),量化原语与本默认值同口径。
pub(crate) const DEFAULT_RESCORE_OVERSAMPLE: usize = 4;

/// 建段抽样召回一致率门槛缺省值:0.98(设计 08 §4.3)。
pub(crate) const DEFAULT_QUANT_RECALL_FLOOR: f32 = 0.98;

/// 进阶调参(通常保持默认)。
#[derive(Debug, Clone, PartialEq)]
pub struct Tuning {
    /// 暴力扫描的计算分块粒度,默认 8192。
    pub parallel_block: usize,
    /// 每段可索引字段上限,默认 16。
    pub field_dict_max: u16,
    /// 布隆过滤器目标误判率,默认 0.01。
    pub bloom_fpp: f32,
    /// 段行数低于此值恒用暴力扫描,默认 2048。
    pub brute_force_max_rows: u32,
    /// 过滤三档:后过滤 / 放大后过滤分界,默认 0.10。
    pub filter_post_threshold: f32,
    /// 过滤三档:放大后过滤 / 候选暴力分界,默认 0.001。
    pub filter_brute_threshold: f32,
    /// 是否启用内置停用词表,默认 `true`。
    ///
    /// **建库即锁定**:打开既有库时以 MANIFEST 记录值为准,本字段仅对新建库生效;
    /// 冲突值会被忽略,以保证索引分词与查询分词同口径(FC-PERSIST-POST-009)。
    pub stopwords: bool,
    /// 两阶段粗排过采样倍率,默认 4。
    ///
    /// 粗排候选数 = `top_k × 本值`(与候选总数取小);再以 f32 原向量精排并按 f32
    /// 分重排取 `top_k`(设计 08 §4.2;`FC-QUANT-INV-015`)。必须 ≥ 1。
    pub rescore_oversample: usize,
    /// 建段抽样召回一致率门槛,默认 0.98。
    ///
    /// 开量化建段时抽样的「量化粗排 top-k 与 f32 精排 top-k 一致率」低于本值时,
    /// 该段自动回退为 f32 并如实反映在 `stats().quant`(I13)。取值须为有限且 ≥ 0;
    /// `0.0` 表示关闭自动回退,`> 1` 表示恒回退(测试/强制关闭用)。
    pub quant_recall_floor: f32,
    /// HNSW 建图启发式选邻的「新方向」比较上限,默认 4。
    ///
    /// 候选按距离升序,只与最近选中的至多该数量比较:调小更快、调大更准
    /// (1536 维实测 8→4 与无上限召回相同、构建快约 1.4×)。必须 ≥ 1。
    pub hnsw_compare_cap: usize,
    /// HNSW 批内并行建图的批行数,默认 128。
    ///
    /// 批大小只依赖节点数、与线程数无关(同输入同图);调大并行任务更多、
    /// 批内互不可见更强。默认值经 4 核基准实测:128 维 / 1536 维均较 8 快
    /// 10%–20%,召回不变;256 起召回出现可测下降。必须 ≥ 1。
    pub hnsw_batch_rows: usize,
    /// HNSW 建图小图串行阈值,默认 64。
    ///
    /// 节点数 ≤ 该值时不分批(无并行收益且避免冷启动批破坏连通性)。必须 ≥ 1。
    pub hnsw_serial_rows: usize,
    /// HNSW 建图批内并行度硬上限,默认 8。
    ///
    /// 实际线程数 = `min(Builder::parallelism(0=可用核数), 本值, 批行数)`。
    /// 必须 ≥ 1。
    pub hnsw_threads_max: usize,
    /// 大 flush 切块行数,默认 65_536。
    ///
    /// 块数 = ⌈行数/本值⌉;每块一个段。环境变量 `MNEME_FLUSH_CHUNK_ROWS`
    /// 优先于本值(测试/调参)。必须 ≥ 1。
    pub flush_chunk_rows: usize,
    /// flush 块级并行度,默认 1(块级串行)。
    ///
    /// 块级并行与块内批并行嵌套会争抢内存带宽(4 核实测反而更慢),故默认串行、
    /// 把并行度交给块内批并行。环境变量 `MNEME_FLUSH_THREADS` 优先于本值。
    /// 必须 ≥ 1。
    pub flush_threads: usize,
}

/// HNSW 建图选邻比较上限缺省值(设计 05 §4.3;1536 维实测 4 与无上限同召回)。
pub(crate) const DEFAULT_HNSW_COMPARE_CAP: usize = 4;
/// HNSW 批内建图批行数缺省值(`FC-INDEX-POST-012`;经 4 核实测标定,见字段文档)。
pub(crate) const DEFAULT_HNSW_BATCH_ROWS: usize = 128;
/// HNSW 建图小图串行阈值缺省值。
pub(crate) const DEFAULT_HNSW_SERIAL_ROWS: usize = 64;
/// HNSW 建图批内并行度硬上限缺省值。
pub(crate) const DEFAULT_HNSW_THREADS_MAX: usize = 8;
/// 大 flush 切块行数缺省值。
pub(crate) const DEFAULT_FLUSH_CHUNK_ROWS: usize = 65_536;
/// flush 块级并行度缺省值(1 = 块级串行,并行交给块内批并行)。
pub(crate) const DEFAULT_FLUSH_THREADS: usize = 1;

impl Default for Tuning {
    fn default() -> Self {
        Self {
            parallel_block: 8192,
            field_dict_max: 16,
            bloom_fpp: 0.01,
            brute_force_max_rows: 2048,
            filter_post_threshold: 0.10,
            filter_brute_threshold: 0.001,
            stopwords: true,
            rescore_oversample: DEFAULT_RESCORE_OVERSAMPLE,
            quant_recall_floor: DEFAULT_QUANT_RECALL_FLOOR,
            hnsw_compare_cap: DEFAULT_HNSW_COMPARE_CAP,
            hnsw_batch_rows: DEFAULT_HNSW_BATCH_ROWS,
            hnsw_serial_rows: DEFAULT_HNSW_SERIAL_ROWS,
            hnsw_threads_max: DEFAULT_HNSW_THREADS_MAX,
            flush_chunk_rows: DEFAULT_FLUSH_CHUNK_ROWS,
            flush_threads: DEFAULT_FLUSH_THREADS,
        }
    }
}

/// HNSW 建图距离精度档位(设计 05 §4.4;`FC-INDEX-POST-010/011`)。
///
/// 只影响 HNSW 构建期的距离计算,不改变存储格式与查询语义;flush/compaction
/// 按当前配置生效(与 [`VectorFormat`] 同口径)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BuildPrecision {
    /// 全 f32 精确建图(原行为;逐位可复现)。
    F32,
    /// 默认:建图遍历用段内临时 i8 码流近似距离(读带宽 ÷4),
    /// 邻居选择/修剪前按 f32 原向量对候选精排;临时码流不落盘。
    #[default]
    Hybrid,
}

/// HNSW 建图工程参数(由 [`Tuning`] 派生;批内并行、选邻比较上限等)。
///
/// 与算法参数 [`HnswParams`] 分开:这些只影响构建过程的并行与近似策略,
/// 不改变磁盘格式与查询语义(`FC-INDEX-POST-012`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HnswBuildParams {
    /// 批内并行线程数(`0` = 按可用核数)。
    pub(crate) parallelism: usize,
    /// 批内并行度硬上限。
    pub(crate) threads_max: usize,
    /// 批行数。
    pub(crate) batch_rows: usize,
    /// 小图串行阈值。
    pub(crate) serial_rows: usize,
    /// 选邻「新方向」比较上限。
    pub(crate) compare_cap: usize,
}

impl HnswBuildParams {
    /// 由进阶调参与实际并行度派生。
    pub(crate) fn from_tuning(tuning: &Tuning, parallelism: usize) -> Self {
        Self {
            parallelism,
            threads_max: tuning.hnsw_threads_max,
            batch_rows: tuning.hnsw_batch_rows,
            serial_rows: tuning.hnsw_serial_rows,
            compare_cap: tuning.hnsw_compare_cap,
        }
    }
}

impl Default for HnswBuildParams {
    fn default() -> Self {
        Self {
            parallelism: 0,
            threads_max: DEFAULT_HNSW_THREADS_MAX,
            batch_rows: DEFAULT_HNSW_BATCH_ROWS,
            serial_rows: DEFAULT_HNSW_SERIAL_ROWS,
            compare_cap: DEFAULT_HNSW_COMPARE_CAP,
        }
    }
}

/// 向量量化格式(量化实现见 L6)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorFormat {
    /// 仅 f32 原向量(默认)。
    #[default]
    F32,
    /// 额外维护 f16 量化副本(feature `quant-f16`)。
    F16,
    /// 额外维护 i8 量化副本 + 两阶段重打分。
    I8Rescored,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_defaults_match_design() {
        let hnsw = HnswParams::default();
        assert_eq!(
            (hnsw.m, hnsw.m0, hnsw.ef_construction, hnsw.ef_search),
            (16, 32, 200, 64)
        );
        assert_eq!(VectorFormat::default(), VectorFormat::F32);
    }

    /// FC-INDEX-PRE-001 / FC-INDEX-POST-012:建图/建段工程调参默认值与设计一致
    /// (选邻比较上限 4、批 128、小图阈值 64、建图线程上限 8、块行数 65_536、
    /// 块级串行 1)。
    #[test]
    fn build_tuning_defaults_match_design() {
        let tuning = Tuning::default();
        assert_eq!(tuning.hnsw_compare_cap, DEFAULT_HNSW_COMPARE_CAP);
        assert_eq!(tuning.hnsw_batch_rows, DEFAULT_HNSW_BATCH_ROWS);
        assert_eq!(tuning.hnsw_serial_rows, DEFAULT_HNSW_SERIAL_ROWS);
        assert_eq!(tuning.hnsw_threads_max, DEFAULT_HNSW_THREADS_MAX);
        assert_eq!(tuning.flush_chunk_rows, DEFAULT_FLUSH_CHUNK_ROWS);
        assert_eq!(tuning.flush_threads, DEFAULT_FLUSH_THREADS);
        assert_eq!(tuning.hnsw_compare_cap, 4, "1536 维实测默认值");
        assert_eq!(tuning.hnsw_batch_rows, 128, "4 核基准标定默认值");
        assert_eq!(tuning.flush_threads, 1, "块级默认串行");
        let build = HnswBuildParams::from_tuning(&tuning, 0);
        assert_eq!(
            (
                build.parallelism,
                build.threads_max,
                build.batch_rows,
                build.serial_rows,
                build.compare_cap
            ),
            (0, 8, 128, 64, 4)
        );
    }

    /// FC-QUANT-PRE-001 / FC-QUANT-INV-015:量化调参默认值与设计一致
    /// (不因重构漂移;0.0 关回退、>1 恒回退的语义见字段文档)。
    #[test]
    fn tuning_quantization_defaults_match_design() {
        let tuning = Tuning::default();
        assert_eq!(
            tuning.rescore_oversample, DEFAULT_RESCORE_OVERSAMPLE,
            "默认过采样倍率为 4"
        );
        assert_eq!(
            tuning.quant_recall_floor, DEFAULT_QUANT_RECALL_FLOOR,
            "默认召回门槛为 0.98"
        );
        assert!((tuning.quant_recall_floor - 0.98).abs() < f32::EPSILON);
    }
}
