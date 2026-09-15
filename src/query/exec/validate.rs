//! 执行前校验与视图准备:通道、维度、上限、融合、多样性与去重参数(设计 06 §5)。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::options::Diversity;
use crate::core::types::NsId;
use crate::memory::Fusion;
use crate::memory::dedup::ResultDedup;
use crate::memory::search_builder::SearchBuilder;
use crate::memory::table::ReaderView;
use crate::memory::temporal;

impl SearchBuilder<'_> {
    /// 取检索视图:优先钉住的快照,否则取当前读视图;校验关闭态与通道非空。
    pub(super) fn prepare_view(&self) -> Result<Arc<ReaderView>> {
        let view = match &self.pinned {
            Some(view) => Arc::clone(view),
            None => self.table.view(),
        };
        if view.closed {
            return Err(MnemeError::Closed);
        }
        if self.vector.is_none() && self.text.is_none() {
            return Err(MnemeError::Config {
                reason: "检索至少需要一个查询通道",
            });
        }
        Ok(view)
    }

    /// 校验查询向量维度与 `top_k`/`ef` 上限。
    pub(super) fn validate_query(&self) -> Result<()> {
        if let Some(query) = &self.vector {
            let expected = self.config.dimension.get() as usize;
            if query.len() != expected {
                return Err(MnemeError::DimensionMismatch {
                    expected: self.config.dimension.get(),
                    got: query.len(),
                });
            }
        }
        if self.top_k > self.config.limits.top_k_max as usize {
            return Err(MnemeError::LimitExceeded {
                field: "top_k",
                limit: self.config.limits.top_k_max as usize,
                got: self.top_k,
            });
        }
        if let Some(ef) = self.ef
            && ef > self.config.limits.ef_max as usize
        {
            return Err(MnemeError::LimitExceeded {
                field: "ef",
                limit: self.config.limits.ef_max as usize,
                got: ef,
            });
        }
        Ok(())
    }

    /// 校验融合配置:融合需要双通道(`Fusion` 单独设置即拒绝,绝不静默忽略);
    /// `Weighted.alpha` 须在 `[0,1]` 内且为有限值(NaN 被区间判定拒绝)。
    pub(super) fn validate_fusion(&self) -> Result<()> {
        let dual = self.vector.is_some() && self.text.is_some();
        if self.fusion.is_some() && !dual {
            return Err(MnemeError::Config {
                reason: "Fusion 需要向量与文本两个通道",
            });
        }
        if let Some(Fusion::Weighted { alpha }) = self.fusion
            && !(0.0..=1.0).contains(&alpha)
        {
            return Err(MnemeError::Config {
                reason: "Fusion::Weighted.alpha 必须是 [0,1] 内的有限值",
            });
        }
        Ok(())
    }

    /// 校验多样性策略参数:非有限 `lambda` 会让 MMR 的 `clamp` 对 NaN 失效并静默退化
    /// (固定取首项),故入口显式拒绝(FC-MEM-PRE-003,拒绝静默失败)。
    pub(super) fn validate_diversify(&self) -> Result<()> {
        if let Diversity::Mmr { lambda } = self.diversify
            && !lambda.is_finite()
        {
            return Err(MnemeError::Config {
                reason: "MMR lambda 必须是 [0,1] 内的有限值",
            });
        }
        Ok(())
    }

    /// 校验结果级去重参数:`Near` 阈值须为 `[0,1]` 内的有限值——`NaN`/越界会让
    /// `cosine_sim >= threshold` 恒假、去重静默空转,入口显式拒绝
    /// (FC-SCORE-POST-005,拒绝静默失败)。
    pub(super) fn validate_dedup(&self) -> Result<()> {
        if let ResultDedup::Near { threshold } = self.dedup
            && !(0.0..=1.0).contains(&threshold)
        {
            return Err(MnemeError::Config {
                reason: "ResultDedup::Near.threshold 必须是 [0,1] 内的有限值",
            });
        }
        Ok(())
    }

    /// 解析命名空间路径对应的 `NsId`(未注册则 `None`)。
    pub(super) fn resolve_ns_id(&self, view: &ReaderView) -> Option<NsId> {
        view.ns_by_path.get(&*self.ns_path).copied()
    }

    /// 指定 `as_of` 时在版本链上重建历史视图。
    pub(super) fn apply_as_of(&self, view: Arc<ReaderView>) -> Arc<ReaderView> {
        match self.as_of {
            Some(ts_ms) => Arc::new(temporal::snapshot_at(&view, ts_ms)),
            None => view,
        }
    }
}
