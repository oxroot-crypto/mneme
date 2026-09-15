//! 段读取后端抽象(`source/`)。
//!
//! 索引层与恢复层只依赖 [`SegmentSource`];`mmap` 是**优化**而非功能依赖
//! (设计 04 §11)。按依赖白名单(01 §5),`memmap2`/`MmapSource` 自 L3 引入:
//! feature `mmap`(默认开)经 [`MmapSource`] 零拷贝映射,关闭时回退本层基于
//! `std`(`Read + Seek`)的 [`FileSource`]——功能完整,吞吐稍低。
//!
//! 自段句柄惰性驻留(FC-PERSIST-INV-021)起,打开路径用 [`SegmentHandle`]
//! 长期持有段文件:[`ByteFile`] 实现 L1 [`ByteSource`](crate::memory::lazy::ByteSource),
//! 向量/量化码/图邻接按需切片;非 mmap 构建整文件读入自有缓冲,功能等价。
//!
//! # 子模块
//!
//! * `backend` —— [`SegmentSource`] 抽象与 `FileSource`/`MmapSource` 实现。
//! * `byte_file` —— [`ByteFile`] 段文件句柄。
//! * `handle` —— [`SegmentHandle`] 三文件句柄集。

mod backend;
mod byte_file;
mod handle;

#[cfg(test)]
mod tests;

// 旧路径 `crate::persist::source::X` 要保持可达;crate 里只在测试下经旧路径使用,放行警告。
#[cfg(all(feature = "mmap", not(feature = "wasm")))]
pub(crate) use backend::map_readonly;
#[allow(unused_imports)]
pub(crate) use backend::{FileSource, SegmentSource, read_whole};
// `MmapSource` 拆分后仅测试经旧路径使用(保持定义与可见性不变)。
#[cfg(all(test, feature = "mmap", not(feature = "wasm")))]
pub(crate) use backend::MmapSource;
pub(crate) use byte_file::ByteFile;
pub(crate) use handle::{SegmentHandle, SegmentHandleOpenInput};
