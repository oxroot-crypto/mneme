//! 建库入口 [`Builder::build`] 与后端打开、配置定型、物理表装配。

use std::sync::Arc;

use crate::core::error::{MnemeError, Result};
use crate::core::metric::Metric;
use crate::core::options::{Dimension, MonotonicClock};
use crate::memory::config::Config;
use crate::memory::engine::Mneme;
use crate::memory::ops::CompactionControl;
use crate::memory::table::{PersistHook, Table, WriterState};
use crate::persist::store::{OpenOptions, Store};

use super::model::{Builder, OpenedBackend};

impl Builder {
    /// 构建库句柄。
    ///
    /// # 既存库配置
    /// 打开既有库时,`Tuning::stopwords` 以 MANIFEST 记录值为准(建库即锁定),
    /// 调用方冲突配置被忽略,避免索引分词与查询分词不一致(FC-PERSIST-POST-009)。
    ///
    /// # Returns
    ///
    /// 打开(或新建)的库句柄 [`Mneme`]。
    ///
    /// # Errors
    /// * 未设置 `dimension`(新建库)→ [`MnemeError::Config`];
    /// * `dedup_threshold` 非 `[0,1]` 内的有限值 → [`MnemeError::Config`]
    ///   (FC-GLOBAL-PRE-004:NaN 会让去重静默失效,越界值超出余弦相似度口径,绝不静默);
    /// * HNSW 参数域非法(`m < 2`/`m0 < m`/`ef_construction = 0`/`ef_search = 0`)或过滤阈值
    ///   非法(非有限值、越界、`brute > post`)→ [`MnemeError::Config`];`ef_search` 或度数
    ///   超上限 → [`MnemeError::LimitExceeded`](FC-INDEX-PRE-001);
    /// * `path` 已存在库且显式维度/度量与其不符 → [`MnemeError::DimensionMismatch`]/
    ///   [`MnemeError::MetricMismatch`];目录被其他实例独占 → [`MnemeError::Busy`]。
    ///
    /// # Examples
    /// ```
    /// use mneme::Builder;
    /// let db = Builder::default().dimension(2).build().unwrap();
    /// assert!(db.list_namespaces().unwrap().is_empty());
    /// ```
    pub fn build(mut self) -> Result<Mneme> {
        self.validate()?;
        let (store, recovered, dimension, metric) = self.open_backend()?;
        // 既存库的分词口径以 MANIFEST 为准(建库即锁定):调用方冲突配置
        // 只影响本次查询会造成索引/查询分词不一致,静默漏召回。
        if let Some(state) = &recovered {
            self.tuning.stopwords = state.stopwords_enabled;
        }
        let read_only_probe_interval = self.read_only_probe_interval;
        // 后台维护开关需在 `into_config` 消费 `self` 之前取出(`FC-LIFE-POST-010`)。
        let maintenance = self.maintenance;
        let config = Arc::new(self.into_config(dimension, metric));
        let hook = store.as_ref();
        let table = build_table(hook, recovered, &config);
        let mut db = Mneme {
            table,
            config,
            control: CompactionControl::new(),
            store,
            maintenance: None,
        };
        // 后台维护:持久库(访问攒批/自动 compaction)或显式开启自动遗忘的纯内存库;
        // `maintenance(false)` 时整体不启动(手动 `maintenance_tick`/`compact` 照常)。
        if maintenance
            && !db.config.read_only
            && (db.store.is_some() || db.config.retention.is_some())
        {
            db.maintenance = Some(crate::life::maintenance::spawn(
                &db.table,
                &db.config,
                &db.control,
                db.store.as_ref(),
            ));
        }
        // 只读共享:周期探测 `current` 并原子换视图(设计 12 §2.1;0 = 关闭)。
        if db.config.read_only
            && !read_only_probe_interval.is_zero()
            && let Some(store) = db.store.as_ref()
        {
            db.maintenance = Some(crate::life::maintenance::spawn_read_only_probe(
                &db.table,
                store,
                read_only_probe_interval,
            ));
        }
        Ok(db)
    }

    /// 打开持久后端;纯内存库返回 `None` 后端与初始写状态。
    ///
    /// 有 `path` 走持久层;否则纯内存。持久路径先解析维度/度量(已存在库以
    /// MANIFEST 为准),再统一构造配置。
    pub(super) fn open_backend(&self) -> Result<OpenedBackend> {
        let requested_metric = self.metric_explicit.then_some(self.metric);
        match &self.path {
            Some(path) => {
                let (store, state, dimension, metric) = Store::open(
                    path,
                    OpenOptions {
                        dimension: self.dimension,
                        metric: requested_metric,
                        fsync: self.fsync,
                        read_only: self.read_only,
                        verify_on_open: self.verify_on_open,
                        fail_fast_on_corruption: self.fail_fast_on_corruption,
                        hook: self.fsync_hook.clone(),
                        index_factory: Some(crate::index::default_factory()),
                        wal_file_bytes: self.compaction.wal_file_bytes,
                        tuning: self.tuning.clone(),
                        compression: self.compression,
                        encryption: self.encryption.clone(),
                        storage: self.storage.clone(),
                        observer: self.observer.clone(),
                        limits: self.limits,
                    },
                )?;
                Ok((Some(store), Some(state), Dimension::new(dimension)?, metric))
            }
            None => {
                let dimension = self.dimension.ok_or(MnemeError::Config {
                    reason: "新建内存库必须指定维度",
                })?;
                Ok((None, None, Dimension::new(dimension)?, self.metric))
            }
        }
    }

    /// 消耗 `Builder` 构造不可变运行配置。
    pub(super) fn into_config(self, dimension: Dimension, metric: Metric) -> Config {
        Config {
            dimension,
            metric,
            insert_mode: self.insert_mode,
            dedup: self.dedup,
            dedup_threshold: self.dedup_threshold,
            quantization: self.quantization,
            hnsw: self.hnsw,
            build_precision: self.build_precision,
            index_factory: Some(crate::index::default_factory()),
            compaction: self.compaction,
            retention: self.retention,
            retain_interval: self.retain_interval,
            access_flush_interval: self.access_flush_interval,
            compression: self.compression,
            observer: self.observer,
            relation_index: self.relation_index,
            parallelism: self.parallelism,
            tuning: self.tuning,
            limits: self.limits,
            // 时钟回拨单调钳制:TTL 只可能晚消失(FC-GLOBAL-PRE-005)。
            clock: Arc::new(MonotonicClock::new(self.clock)),
            read_only: self.read_only,
        }
    }
}

/// 由持久后端与写状态构造物理表。
pub(super) fn build_table(
    store: Option<&Arc<Store>>,
    recovered: Option<WriterState>,
    config: &Arc<Config>,
) -> Arc<Table> {
    match (store, recovered) {
        (Some(store), Some(state)) => {
            let hook: Arc<dyn PersistHook> = Arc::clone(store) as Arc<dyn PersistHook>;
            Arc::new(Table::from_state(Arc::clone(config), state, Some(hook)))
        }
        _ => Arc::new(Table::new(Arc::clone(config))),
    }
}
