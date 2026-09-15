//! 写入语义与索引相关 setter。

use crate::core::options::{BuildPrecision, HnswParams, InsertMode, RelationIndex, VectorFormat};
use crate::memory::dedup::Dedup;

use super::super::Builder;

impl Builder {
    /// 设置同 key 写入行为。
    ///
    /// # Arguments
    ///
    /// * `insert_mode` - `Upsert`(覆盖)或 `RejectDuplicate`(拒绝重复)。
    ///
    /// # Returns
    ///
    /// 携带写入行为的构建器(链式)。
    pub fn insert_mode(mut self, insert_mode: InsertMode) -> Self {
        self.insert_mode = insert_mode;
        self
    }

    /// 设置写入期去重策略。
    ///
    /// # Arguments
    ///
    /// * `dedup` - 去重策略(可携带 `Merge` 回调)。
    ///
    /// # Returns
    ///
    /// 携带去重策略的构建器(链式)。
    pub fn dedup(mut self, dedup: Dedup) -> Self {
        self.dedup = dedup;
        self
    }

    /// 设置近似去重阈值(默认 0.95,统一按余弦口径)。
    ///
    /// # Arguments
    ///
    /// * `threshold` - 余弦相似度阈值,`[0,1]`;越界或非有限值在 `build` 入口拒绝。
    ///
    /// # Returns
    ///
    /// 携带阈值的构建器(链式)。
    pub fn dedup_threshold(mut self, threshold: f32) -> Self {
        self.dedup_threshold = threshold;
        self
    }

    /// 设置量化格式(仅记录,L6 生效)。
    ///
    /// # Arguments
    ///
    /// * `quantization` - 向量量化格式。
    ///
    /// # Returns
    ///
    /// 携带量化格式的构建器(链式)。
    pub fn quantization(mut self, quantization: VectorFormat) -> Self {
        self.quantization = quantization;
        self
    }

    /// 设置 HNSW 参数(L3 生效)。
    ///
    /// # Arguments
    ///
    /// * `hnsw` - 图参数。
    ///
    /// # Returns
    ///
    /// 携带 HNSW 参数的构建器(链式)。
    pub fn hnsw(mut self, hnsw: HnswParams) -> Self {
        self.hnsw = hnsw;
        self
    }

    /// 设置 HNSW 建图距离精度档位(默认 [`BuildPrecision::Hybrid`])。
    ///
    /// 只影响 flush/compaction 的**新段**构建距离;不改变磁盘格式与查询语义。
    /// `Hybrid` 用段内临时 i8 码流近似遍历、选邻前 f32 精排;`F32` 为全精确原行为。
    ///
    /// # Arguments
    ///
    /// * `precision` - 建图精度档位。
    ///
    /// # Returns
    ///
    /// 携带建图精度的构建器(链式)。
    ///
    /// # Examples
    /// ```
    /// use mneme::{Builder, BuildPrecision};
    /// let db = Builder::default()
    ///     .dimension(2)
    ///     .build_precision(BuildPrecision::F32)
    ///     .build()
    ///     .unwrap();
    /// let _ = db.namespace("demo");
    /// ```
    pub fn build_precision(mut self, precision: BuildPrecision) -> Self {
        self.build_precision = precision;
        self
    }

    /// 设置关系邻接索引方向。
    ///
    /// # Arguments
    ///
    /// * `relation_index` - `Outgoing`(仅出边)或 `Both`(出边 + 反向)。
    ///
    /// # Returns
    ///
    /// 携带索引方向的构建器(链式)。
    pub fn relation_index(mut self, relation_index: RelationIndex) -> Self {
        self.relation_index = relation_index;
        self
    }
}
