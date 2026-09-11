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
}

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
}
