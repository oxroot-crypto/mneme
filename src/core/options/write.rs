//! 写入与更新路径的语义配置。
//!
//! 覆盖落盘同步策略、同 key 写入行为、局部更新补丁与文本 / 元数据压缩;
//! fsync 语义与权衡见设计 00 §6.1 与 04 §3,更新补丁字段语义见设计 03 §2.1。

use std::time::Duration;

use crate::core::meta::Meta;

/// 批量 fsync 的缺省时间窗口(毫秒;来源:设计 00 §6.1)。
const DEFAULT_FSYNC_BATCH_MS: u64 = 20;

/// 落盘同步(fsync)策略。
///
/// 语义与权衡见设计 00 §6.1 与 04 §3。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsyncPolicy {
    /// 每次写入后立即 fsync。
    Always,
    /// 按时间窗口批量 fsync。
    Batched(Duration),
    /// 仅在显式 `flush()` 时 fsync。
    OnFlush,
    /// 从不 fsync(**仅供测试**)。
    Never,
}

impl Default for FsyncPolicy {
    fn default() -> Self {
        Self::Batched(Duration::from_millis(DEFAULT_FSYNC_BATCH_MS))
    }
}

/// 同一 key 的写入行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InsertMode {
    /// 覆盖既有记录(默认)。
    #[default]
    Upsert,
    /// 重复 key 返回
    /// [`MnemeError::DuplicateKey`](crate::core::error::MnemeError::DuplicateKey)。
    RejectDuplicate,
}

/// 局部更新补丁;外层 `None` = 不改动该字段,`Some(None)` = 清空。
///
/// 字段语义见设计 03 §2.1。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UpdatePatch {
    /// 新向量;`None` = 不改动。
    pub vector: Option<Vec<f32>>,
    /// 新文本;`Some(None)` = 清空。
    pub text: Option<Option<String>>,
    /// 新元数据;`Some(None)` = 清空,`Some(Some(v))` = 整体替换。
    pub metadata: Option<Option<Meta>>,
    /// 新重要度。
    pub importance: Option<f32>,
    /// 新 TTL;`Some(None)` = 取消过期。
    pub ttl: Option<Option<Duration>>,
    /// 新有效时间区间 `(valid_from, valid_to)`。
    pub valid_time: Option<(i64, Option<i64>)>,
    /// 新可信度。
    pub confidence: Option<f32>,
    /// 新来源 / 派生链;`Some(None)` = 清空。
    pub provenance: Option<Option<Meta>>,
}

impl UpdatePatch {
    /// 返回一个不改动任何字段的空补丁。
    ///
    /// # Returns
    ///
    /// 所有字段均为 `None` 的空补丁,等价于 [`UpdatePatch::default`]。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置新向量(链式)。
    ///
    /// # Arguments
    ///
    /// * `vector` - 新向量;维度与有限性在 `update` 入口校验。
    ///
    /// # Returns
    ///
    /// 携带新向量的补丁(链式)。
    pub fn vector(mut self, vector: Vec<f32>) -> Self {
        self.vector = Some(vector);
        self
    }

    /// 设置新文本;`Some(None)` 清空。
    ///
    /// # Arguments
    ///
    /// * `text` - 新文本;`None` 表示清空该字段。
    ///
    /// # Returns
    ///
    /// 携带文本变更的补丁(链式)。
    pub fn text(mut self, text: Option<String>) -> Self {
        self.text = Some(text);
        self
    }

    /// 设置新元数据;`Some(None)` 清空。
    ///
    /// # Arguments
    ///
    /// * `metadata` - 新元数据;`None` 表示清空该字段。
    ///
    /// # Returns
    ///
    /// 携带元数据变更的补丁(链式)。
    pub fn metadata(mut self, metadata: Option<Meta>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// 设置新重要度。
    ///
    /// # Arguments
    ///
    /// * `importance` - 新重要度,`[0,1]`,越界钳制;非有限值(NaN)在 `update` 入口拒绝。
    ///
    /// # Returns
    ///
    /// 携带重要度的补丁(链式)。
    pub fn importance(mut self, importance: f32) -> Self {
        self.importance = Some(importance);
        self
    }

    /// 设置新 TTL;`Some(None)` 取消过期。
    ///
    /// # Arguments
    ///
    /// * `ttl` - 新 TTL;`None` 表示取消过期。
    ///
    /// # Returns
    ///
    /// 携带 TTL 变更的补丁(链式)。
    pub fn ttl(mut self, ttl: Option<Duration>) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// 设置新有效时间区间。
    ///
    /// # Arguments
    ///
    /// * `from` - 有效时间起(Unix 毫秒)。
    /// * `to` - 有效时间止(开区间);`None` = 开放右端。
    ///
    /// # Returns
    ///
    /// 携带有效时间区间的补丁(链式)。
    pub fn valid_time(mut self, from: i64, to: Option<i64>) -> Self {
        self.valid_time = Some((from, to));
        self
    }

    /// 设置新可信度。
    ///
    /// # Arguments
    ///
    /// * `confidence` - 新可信度,`[0,1]`,越界钳制;非有限值(NaN)在 `update` 入口拒绝。
    ///
    /// # Returns
    ///
    /// 携带可信度的补丁(链式)。
    pub fn confidence(mut self, confidence: f32) -> Self {
        self.confidence = Some(confidence);
        self
    }

    /// 设置新来源/派生链;`Some(None)` 清空。
    ///
    /// # Arguments
    ///
    /// * `provenance` - 新来源/派生链;`None` 表示清空该字段。
    ///
    /// # Returns
    ///
    /// 携带来源/派生链的补丁(链式)。
    pub fn provenance(mut self, provenance: Option<Meta>) -> Self {
        self.provenance = Some(provenance);
        self
    }
}

/// 文本 / 元数据压缩策略(实现见 L2 可选 feature)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compression {
    /// 不压缩(默认)。
    #[default]
    None,
    /// 内置 LZ4 风格压缩(feature `compress`)。
    Lz4,
    /// Zstd 压缩(feature `compress-zstd`)。
    Zstd,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_path_defaults_match_design() {
        assert_eq!(
            FsyncPolicy::default(),
            FsyncPolicy::Batched(Duration::from_millis(20))
        );
        assert_eq!(InsertMode::default(), InsertMode::Upsert);
    }

    #[test]
    fn update_patch_default_changes_nothing() {
        let patch = UpdatePatch::new();
        assert!(patch.vector.is_none());
        assert!(patch.text.is_none());
        assert!(patch.metadata.is_none());
        assert!(patch.importance.is_none());
        assert!(patch.ttl.is_none());
        assert!(patch.valid_time.is_none());
        assert!(patch.confidence.is_none());
        assert!(patch.provenance.is_none());
    }
}
