//! 快照句柄与快照上的只读命名空间视图(`snapshot/`)。

mod filter;
mod handle;
mod namespace;
mod read;

pub(crate) use filter::passes_filter;
pub use handle::SnapshotHandle;
pub use namespace::SnapshotNamespace;
