//! L2 持久层:文件字节布局、WAL、段文件、MANIFEST 与崩溃恢复。
//!
//! 本层把 L1 的内存引擎升级为"重启不丢数据、可崩溃恢复"的本地库:
//! 定义 `vsec`(向量段)/`msec`(元数据段)/`WAL`/`MANIFEST` 的**字节级格式**,
//! 实现追加式 WAL、write-once MANIFEST、段级 CRC 与统一可见性合并。
//!
//! # 模块
//!
//! * `codec` —— 版本门禁、CRC、带界游标与定长写入辅助。
//! * `vsec` —— 向量段编解码(头部 + 向量区 + norm 区 + 删除位图 + payload CRC)。
//! * `msec` —— 元数据段编解码(头部 + 记录体 + 版本链 + key 索引)。
//! * `edges` —— 关系邻接索引编解码。
//! * `wal` —— WAL 帧编解码、回放与写入器。
//! * `manifest` —— MANIFEST 编解码(write-once + `current` 指针)。
//! * `source` —— 段读取后端抽象(`SegmentSource` 与 `FileSource`/`MmapSource`)。
//! * `storage` —— 目录布局、原子写入与独占文件锁。
//! * `trash` —— 旧段文件的延迟删除。
//! * `flush` —— 写状态 → 增量段文件(未落盘槽位 + delta)。
//! * `recover` —— 段文件 + WAL → 写状态。
//! * `store` —— 协调句柄 `Store`(实现 `PersistHook`、`flush`、`open`)。
//!
//! > 本层向下依赖 [`crate::core`] 与 [`crate::memory`](L1,L2 高于 L1);
//! > `memmap2`/`MmapSource`(feature `mmap`,默认开)自 L3 引入并按白名单受控,
//! > 未启用 feature 时 `source` 回退到基于 `std` 的 `FileSource`。

mod codec;
pub(crate) mod edges;
pub(crate) mod flush;
pub(crate) mod hook;
pub(crate) mod manifest;
pub(crate) mod msec;
pub(crate) mod recover;
// L3 段读取后端(`SegmentSource`/`FileSource`/`MmapSource`,设计 04 §11):
// `read_whole` 统一经此读取段字节;`FileSource`/`MmapSource` 随 feature `mmap`
// 二选一(各后端个未用分支在模块内定点豁免)。
pub(crate) mod source;
pub mod storage;
pub(crate) mod store;
pub(crate) mod trash;
pub(crate) mod vsec;
pub(crate) mod wal;

pub(crate) use codec::{
    Cursor, FORMAT_VERSION, align_up, check_version, crc32, put_bytes_u32, put_i64, put_u16,
    put_u32, put_u64,
};
