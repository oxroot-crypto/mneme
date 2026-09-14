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

/// heavy 门槛默认规模(冒烟):50_000×128,普通开发机秒级到分钟级完成。
///
/// 正式门槛(1M×1536)由 CI heavy 档以 `MNEME_HEAVY_ROWS`/`MNEME_HEAVY_DIM` 指定。
pub const HEAVY_DEFAULT_ROWS: usize = 50_000;
/// heavy 门槛默认向量维度(见 [`HEAVY_DEFAULT_ROWS`])。
pub const HEAVY_DEFAULT_DIMENSION: usize = 128;

/// 读取 `MNEME_HEAVY_ROWS`(缺省 [`HEAVY_DEFAULT_ROWS`]);非法值显式失败。
pub fn heavy_rows() -> usize {
    env_usize("MNEME_HEAVY_ROWS", HEAVY_DEFAULT_ROWS)
}

/// 读取 `MNEME_HEAVY_DIM`(缺省 [`HEAVY_DEFAULT_DIMENSION`]);非法值显式失败。
pub fn heavy_dimension() -> usize {
    env_usize("MNEME_HEAVY_DIM", HEAVY_DEFAULT_DIMENSION)
}

/// 从环境变量读取正整数;未设置用 `default`,配错不得静默回退。
fn env_usize(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .unwrap_or_else(|_| panic!("{name} 非法(需为正整数): {value:?}")),
        Err(_) => default,
    }
}

/// 确定性伪随机向量(线性同余;避免引入 `rand`),heavy 用例共用。
pub fn heavy_vector(seed: u64, dimension: usize) -> Vec<f32> {
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (0..dimension)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f32 / (1_u64 << 31) as f32) - 0.5
        })
        .collect()
}

/// 段整读监控后端:记录 `segments/` 前缀下的 `read_file` 调用(整读),
/// 其余操作委托内部 [`mneme::FsStorage`];用于守护惰性段驻留契约
/// (`FC-PERSIST-INV-021`/`FC-PERSIST-CPLX-007`)。
#[derive(Debug)]
pub struct SegmentReadSpy {
    inner: mneme::FsStorage,
    /// 被整读的段文件相对路径(按发生顺序)。
    pub full_reads: Arc<Mutex<Vec<String>>>,
}

impl SegmentReadSpy {
    /// 以库根目录建立监控后端。
    pub fn new(root: &std::path::Path) -> Self {
        Self {
            inner: mneme::FsStorage::new(root),
            full_reads: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl mneme::Storage for SegmentReadSpy {
    fn read_file(&self, rel: &str) -> mneme::Result<Vec<u8>> {
        if rel.starts_with("segments/") {
            self.full_reads.lock().expect("lock").push(rel.to_string());
        }
        self.inner.read_file(rel)
    }

    fn read_file_opt(&self, rel: &str) -> mneme::Result<Option<Vec<u8>>> {
        self.inner.read_file_opt(rel)
    }

    fn read_prefix(&self, rel: &str, max: usize) -> mneme::Result<Vec<u8>> {
        self.inner.read_prefix(rel, max)
    }

    fn write_atomic(&self, rel: &str, bytes: &[u8]) -> mneme::Result<()> {
        self.inner.write_atomic(rel, bytes)
    }

    fn write_new(&self, rel: &str, bytes: &[u8]) -> mneme::Result<()> {
        self.inner.write_new(rel, bytes)
    }

    fn append(&self, rel: &str, bytes: &[u8]) -> mneme::Result<u64> {
        self.inner.append(rel, bytes)
    }

    fn truncate(&self, rel: &str, len: u64) -> mneme::Result<()> {
        self.inner.truncate(rel, len)
    }

    fn sync(&self, rel: &str) -> mneme::Result<()> {
        self.inner.sync(rel)
    }

    fn list_dir(&self, rel: &str) -> mneme::Result<Vec<String>> {
        self.inner.list_dir(rel)
    }

    fn ensure_dir(&self, rel: &str) -> mneme::Result<()> {
        self.inner.ensure_dir(rel)
    }

    fn remove_if_exists(&self, rel: &str) -> mneme::Result<()> {
        self.inner.remove_if_exists(rel)
    }

    fn rename(&self, from: &str, to: &str) -> mneme::Result<()> {
        self.inner.rename(from, to)
    }

    fn exists(&self, rel: &str) -> mneme::Result<bool> {
        self.inner.exists(rel)
    }

    fn stat(&self, rel: &str) -> mneme::Result<mneme::FileMeta> {
        self.inner.stat(rel)
    }

    fn open_bytes(&self, rel: &str) -> mneme::Result<mneme::RawBytes> {
        self.inner.open_bytes(rel)
    }

    fn root_exists(&self) -> mneme::Result<bool> {
        self.inner.root_exists()
    }

    fn try_lock(&self) -> mneme::Result<Box<dyn std::any::Any + Send + Sync>> {
        self.inner.try_lock()
    }
}
