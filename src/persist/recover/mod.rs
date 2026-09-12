//! 崩溃恢复:段文件 + WAL → 内存写状态(设计 04 §7)。
//!
//! 恢复把各活跃段的 `vsec`/`msec` 重建成版本链、key 索引与关系边,再回放
//! `seqno > watermark` 的 WAL 帧。墓碑以 `version_table.doc_offset =
//! TOMBSTONE_DOC_OFFSET` 表示(无记录体),重建为 `deleted = true` 的槽位,
//! 保证"删除永不复活"(I19)。
//!
//! # 子模块
//!
//! * `state` —— 段视图重建与写状态初始化。
//! * `segment` —— 段解析与版本链/关系边重建辅助。
//! * `replay` —— WAL 帧回放与批原子边界校验。
//! * `wal_replay` —— 单帧应用(供 `replay` 调用)。

mod replay;
mod segment;
mod state;
mod wal_replay;

pub(crate) use replay::replay_wal;
pub(crate) use state::{RecoveredSegments, SegmentBytes, empty_state, load_segments};
