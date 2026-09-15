//! 建库器跨字段配置校验(FC-GLOBAL-PRE-004、FC-INDEX-PRE-001)。

use crate::core::error::{MnemeError, Result};
use crate::core::options::VectorFormat;

use super::model::Builder;

impl Builder {
    /// 校验跨字段配置约束(FC-GLOBAL-PRE-004、FC-INDEX-PRE-001)。
    pub(super) fn validate(&self) -> Result<()> {
        // NaN 的 `contains` 恒为 false,一个区间判断即可同时覆盖非有限值与越界。
        if !(0.0..=1.0).contains(&self.dedup_threshold) {
            return Err(MnemeError::Config {
                reason: "dedup_threshold 必须是 [0,1] 内的有限值",
            });
        }
        self.validate_hnsw()?;
        self.validate_tuning()?;
        self.validate_compaction()?;
        self.validate_quantization()?;
        self.validate_compression()?;
        crate::crypto::ensure_supported(self.encryption.as_ref())?;
        Ok(())
    }

    /// 校验 compaction 策略(FC-LIFE-PRE-001):分级比/阈值至少 2、段初始行数至少 1、
    /// 死比率与 IO 配额为 `[0,1]` 内有限值,否则触发条件永假或除零。
    pub(super) fn validate_compaction(&self) -> Result<()> {
        let policy = self.compaction;
        if policy.tier_ratio < 2 || policy.tier_count < 2 {
            return Err(MnemeError::Config {
                reason: "compaction tier_ratio/tier_count 必须 ≥ 2",
            });
        }
        if policy.segment_rows < 1 {
            return Err(MnemeError::Config {
                reason: "compaction segment_rows 必须 ≥ 1",
            });
        }
        if !(0.0..=1.0).contains(&policy.dead_ratio) {
            return Err(MnemeError::Config {
                reason: "compaction dead_ratio 必须是 [0,1] 内的有限值",
            });
        }
        if !(0.0..=1.0).contains(&policy.io_budget) {
            return Err(MnemeError::Config {
                reason: "compaction io_budget 必须是 [0,1] 内的有限值",
            });
        }
        Ok(())
    }

    /// 校验 HNSW 参数:`m ≥ 2`、`m0 ≥ m`、`ef_construction ≥ 1`、`ef_search ∈ [1, ef_max]`、
    /// 度数不超硬上限(FC-INDEX-PRE-001;越限会使自产 hidx 无法读回,或让默认查询宽度
    /// 绕过查询期上限)。
    pub(super) fn validate_hnsw(&self) -> Result<()> {
        let params = self.hnsw;
        if params.m < 2 || params.m0 < params.m {
            return Err(MnemeError::Config {
                reason: "HNSW m 必须 ≥ 2 且 m0 ≥ m",
            });
        }
        if params.ef_construction < 1 {
            return Err(MnemeError::Config {
                reason: "HNSW ef_construction 必须 ≥ 1",
            });
        }
        if params.ef_search < 1 {
            return Err(MnemeError::Config {
                reason: "HNSW ef_search 必须 ≥ 1",
            });
        }
        let ef_max = self.limits.ef_max as usize;
        if params.ef_search as usize > ef_max {
            return Err(MnemeError::LimitExceeded {
                field: "ef_search",
                limit: ef_max,
                got: params.ef_search as usize,
            });
        }
        let max = crate::memory::index::MAX_INDEX_DEGREE as usize;
        if params.m as usize > max || params.m0 as usize > max {
            return Err(MnemeError::LimitExceeded {
                field: "hnsw 度数(m/m0)",
                limit: max,
                got: params.m.max(params.m0) as usize,
            });
        }
        Ok(())
    }

    /// 校验过滤三档阈值与 bloom/字段上限:有限且 `0 ≤ brute ≤ post ≤ 1`;
    /// `bloom_fpp ∈ (0,1)`(否则会生产 `k > 64`、自读不回的段);
    /// `field_dict_max ≥ 1`(至少容纳 key 字符串字段)(FC-INDEX-PRE-001);
    /// 两阶段候选上限 ≥ 1、召回门槛有限且 ≥ 0(FC-QUANT-PRE-001)。
    pub(super) fn validate_tuning(&self) -> Result<()> {
        self.validate_filter_tuning()?;
        self.validate_quant_tuning()?;
        self.validate_build_tuning()
    }

    /// 校验过滤三档阈值与 bloom/字段上限:有限且 `0 ≤ brute ≤ post ≤ 1`;
    /// `bloom_fpp ∈ (0,1)`(否则会生产 `k > 64`、自读不回的段);
    /// `field_dict_max ≥ 1`(至少容纳 key 字符串字段)(FC-INDEX-PRE-001)。
    fn validate_filter_tuning(&self) -> Result<()> {
        let post = self.tuning.filter_post_threshold;
        let brute = self.tuning.filter_brute_threshold;
        if !(0.0..=1.0).contains(&post) || !(0.0..=1.0).contains(&brute) || brute > post {
            return Err(MnemeError::Config {
                reason: "过滤三档阈值必须是 [0,1] 内有限值且 brute ≤ post",
            });
        }
        let fpp = self.tuning.bloom_fpp;
        if !fpp.is_finite() || fpp <= 0.0 || fpp >= 1.0 {
            return Err(MnemeError::Config {
                reason: "bloom_fpp 必须是 (0,1) 内的有限值",
            });
        }
        if self.tuning.field_dict_max < 1 {
            return Err(MnemeError::Config {
                reason: "field_dict_max 至少为 1(需容纳 key 字段)",
            });
        }
        Ok(())
    }

    /// 校验两阶段候选上限 ≥ 1、召回门槛有限且 ≥ 0(FC-QUANT-PRE-001)。
    fn validate_quant_tuning(&self) -> Result<()> {
        if self.tuning.rescore_oversample < 1 {
            return Err(MnemeError::Config {
                reason: "rescore_oversample 必须 ≥ 1",
            });
        }
        let floor = self.tuning.quant_recall_floor;
        if !floor.is_finite() || floor < 0.0 {
            return Err(MnemeError::Config {
                reason: "quant_recall_floor 必须是 ≥ 0 的有限值",
            });
        }
        Ok(())
    }

    /// 校验建图/建段工程调参一律 ≥ 1(FC-INDEX-PRE-001)。
    fn validate_build_tuning(&self) -> Result<()> {
        // 建图/建段工程调参:0 会让批行数/切块/线程数失去意义(除零或空批),
        // 一律拒绝(FC-INDEX-PRE-001)。
        if self.tuning.hnsw_compare_cap < 1 {
            return Err(MnemeError::Config {
                reason: "hnsw_compare_cap 必须 ≥ 1",
            });
        }
        if self.tuning.hnsw_batch_rows < 1 {
            return Err(MnemeError::Config {
                reason: "hnsw_batch_rows 必须 ≥ 1",
            });
        }
        if self.tuning.hnsw_serial_rows < 1 {
            return Err(MnemeError::Config {
                reason: "hnsw_serial_rows 必须 ≥ 1",
            });
        }
        if self.tuning.hnsw_threads_max < 1 {
            return Err(MnemeError::Config {
                reason: "hnsw_threads_max 必须 ≥ 1",
            });
        }
        if self.tuning.flush_chunk_rows < 1 {
            return Err(MnemeError::Config {
                reason: "flush_chunk_rows 必须 ≥ 1",
            });
        }
        if self.tuning.flush_threads < 1 {
            return Err(MnemeError::Config {
                reason: "flush_threads 必须 ≥ 1",
            });
        }
        Ok(())
    }

    /// 校验量化配置:feature 门控与纯内存限制(FC-QUANT-ERR-001/002)。
    ///
    /// 量化副本随段同生同灭:纯内存库没有段,配置量化没有可服务的载体,
    /// 构造期即报 `Unsupported`,绝不静默记配置回显 `active = F32`。
    pub(super) fn validate_quantization(&self) -> Result<()> {
        if self.quantization == VectorFormat::F32 {
            return Ok(());
        }
        crate::quant::ensure_format_supported(self.quantization)?;
        if self.path.is_none() {
            return Err(MnemeError::Unsupported {
                feature: "量化副本(纯内存库无段)",
            });
        }
        Ok(())
    }

    /// 校验压缩策略与 feature 门控:`Lz4` 需 feature `compress`,`Zstd` 需
    /// `compress-zstd`,未开启即 `Unsupported`,绝不静默按 `None` 运行。
    pub(super) fn validate_compression(&self) -> Result<()> {
        crate::compress::codec_for(self.compression).map(|_codec| ())
    }
}
