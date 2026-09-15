//! 记录值类型:待写入记录、只读视图与写入/更新结果(`record/`)。
//!
//! 这些类型是公开 API 的数据载体(设计 16 §1.2),不含存储实现。

mod hit;
mod stored;
mod view;
mod write;

pub use hit::Hit;
pub use stored::StoredRecord;
pub use view::RecordRef;
pub use write::{InsertOutcome, Record, UpdateOutcome};

#[cfg(test)]
mod tests;
