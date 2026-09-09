//! L0 全局配置与选项类型(纯数据定义,不含行为)。
//!
//! 这些类型贯穿全库:L0 只给出数据形状与默认值,具体语义由对应层实现
//! (如 `HnswParams` 在 L3、`CompactionPolicy` 在 L5、`Scoring` 在 L4)。
//! 行为型成员(如 `RelationKind::custom` 的名称注册表)依赖上层状态,留待对应层补充;
//! `Encryption` 及 `KeyProvider` / `Cipher` 因依赖上层 trait 与 `encrypt` feature,
//! 由 L11 安全层提供(见设计 11 §2),不在本模块。
//!
//! # 子模块
//!
//! 按主题拆分以满足单一职责(见设计 02 §8):[`dimension`] 维度、[`write`] 写入与
//! 更新路径、[`index`] 检索与索引参数、[`limits`] 数据限额、[`lifecycle`] 后台合并、
//! [`scoring`] 打分与关系、[`clock`] 时间源。子模块保持私有,类型一律经本模块
//! `pub use` 对外暴露,公共 API 路径不受拆分影响。

mod clock;
mod dimension;
mod index;
mod lifecycle;
mod limits;
mod scoring;
mod write;

pub use clock::{Clock, SystemClock};
pub use dimension::Dimension;
pub use index::{HnswParams, Tuning, VectorFormat};
pub use lifecycle::CompactionPolicy;
pub use limits::Limits;
pub use scoring::{Diversity, Feedback, QueryId, RelationIndex, RelationKind, Scoring, TimeAxis};
pub use write::{Compression, FsyncPolicy, InsertMode, UpdatePatch};
