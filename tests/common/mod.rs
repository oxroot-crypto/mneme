//! 契约测试共享辅助:可注入假时钟与常用断言工具。
//!
//! 供 `tests/` 下的契约测试文件(`core_contracts.rs`、`memory_contracts.rs`、
//! `query_contracts.rs`、`model_contracts.rs`、`life_contracts.rs`)复用;
//! 本模块只提供工具,不承载任何 `#[test]`。各测试 crate 只用到其中一部分,
//! 故对未用条目显式豁免 dead_code(reason: 按 crate 选择性使用,属共享工具)。
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use mneme::{Clock, InsertOutcome, Mneme, RowId};

/// 可注入的假时钟(毫秒)。
#[derive(Clone, Default)]
pub struct FakeClock(Arc<Mutex<i64>>);

impl FakeClock {
    /// 把当前时刻设置为 `ms`(Unix 毫秒)。
    pub fn set(&self, ms: i64) {
        *self.0.lock().expect("clock lock") = ms;
    }
}

impl Clock for FakeClock {
    fn now_unix_ms(&self) -> i64 {
        *self.0.lock().expect("clock lock")
    }
}

/// 新建指定维度的内存库。
pub fn mem(dim: u32) -> Mneme {
    Mneme::in_memory(dim).expect("in_memory")
}

/// 断言写入结果为新建/合并,返回 `RowId`;其余结果视为失败。
pub fn inserted(outcome: InsertOutcome) -> RowId {
    match outcome {
        InsertOutcome::Inserted(id) | InsertOutcome::Merged(id) => id,
        other => panic!("期望写入,得到 {other:?}"),
    }
}

/// 暴力点积参考实现:按分数降序、同分 `rowid` 升序,截取前 `k`。
pub fn reference_dot(query: &[f32], records: &[(u64, Vec<f32>)], k: usize) -> Vec<u64> {
    let mut scored: Vec<(f32, u64)> = records
        .iter()
        .map(|(id, vector)| (mneme::simd::dot(query, vector), *id))
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .expect("finite scores")
            .then(a.1.cmp(&b.1))
    });
    scored.truncate(k);
    scored.into_iter().map(|(_, id)| id).collect()
}
