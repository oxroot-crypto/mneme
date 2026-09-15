//! WAL 打开参数。

use std::sync::Arc;

use crate::core::metric::Metric;
use crate::core::options::FsyncPolicy;
use crate::persist::hook::FsyncHook;

// `WalConfig` 类型本身仍经 `mod.rs` 重导出;字段不能重导出,故字段可见性用
// `pub(in crate::persist::store)` 精确等价原单文件里的 `pub(super)`(原父模块即
// `store`)。
/// WAL 打开参数(维度/度量来自 MANIFEST,建库即锁定)。
pub(in crate::persist::store) struct WalConfig {
    /// 建库维度(写入 WAL 文件头)。
    pub(in crate::persist::store) dimension: u32,
    /// 距离度量(写入 WAL 文件头)。
    pub(in crate::persist::store) metric: Metric,
    /// fsync 策略。
    pub(in crate::persist::store) policy: FsyncPolicy,
    /// 单文件轮转阈值(字节);`0` = 不轮转。
    pub(in crate::persist::store) max_file_bytes: u64,
    /// 崩溃注入钩子。
    pub(in crate::persist::store) hook: Option<Arc<dyn FsyncHook>>,
    /// 只读打开(仅只读句柄、不创建/改写)。
    pub(in crate::persist::store) read_only: bool,
    /// 静态加密配置(`None` = 明文帧)。
    pub(in crate::persist::store) encryption: Option<crate::crypto::Encryption>,
    /// 单帧负载上限(字节;`0` = 不限制);超限写入返回 `LimitExceeded`,
    /// 绝不截断或写出超限帧(FC-PERSIST-ERR-013,见设计 16 §8)。
    pub(in crate::persist::store) frame_max: usize,
}
