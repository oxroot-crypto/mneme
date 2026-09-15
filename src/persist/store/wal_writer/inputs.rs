//! WAL 写入器的打开/组装输入参数(`wal_writer/inputs.rs`)。

use std::sync::Arc;

use crate::persist::storage::Storage;

use super::config::WalConfig;

/// [`WalWriter::from_parts`] 的输入参数。
pub(super) struct FromPartsInput {
    /// 存储后端。
    pub(super) storage: Arc<dyn Storage>,
    /// 活动文件序号。
    pub(super) active_index: u32,
    /// WAL 打开参数(维度/度量/策略/钩子等)。
    pub(super) config: WalConfig,
    /// 是否可写(只读实例为 `false`)。
    pub(super) writable: bool,
    /// 活动文件当前字节数。
    pub(super) written: u64,
    /// 已轮转旧文件字节合计。
    pub(super) sealed: u64,
}

/// [`WalWriter::open_existing`] 的输入参数。
pub(super) struct OpenExistingInput<'a> {
    /// 存储后端。
    pub(super) storage: Arc<dyn Storage>,
    /// 目录内全部 WAL 文件(含活动文件)。
    pub(super) files: &'a [String],
    /// 活动文件相对路径。
    pub(super) rel: &'a str,
    /// 活动文件序号。
    pub(super) index: u32,
    /// WAL 配置。
    pub(super) config: WalConfig,
}
