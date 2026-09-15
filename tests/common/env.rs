//! 开发侧(契约测试 / 示例)环境变量读取统一入口。
//!
//! **Mneme 本体绝不读取环境变量**:库内所有调参与开关都由 dev 侧读入后,经
//! 配置结构体(`Builder` / `Tuning`)或函数参数显式传入(设计 16 §6)。本模块是
//! 测试与示例访问环境变量的唯一通道:变量名一律用下方常量,调用点不散写字面量,
//! 空值与非法值的处理口径全库一致。
//!
//! 测试经 `tests/common/mod.rs` 复用本模块;示例经 `#[path]` 直接复用同一文件
//! (`examples/memory/main.rs`),保证两侧读取语义逐字相同。
//!
//! # 读取顺序
//!
//! 真实环境变量 > 仓库根 `.env`(已 gitignore;仅 dev 目标读取,库本体不读)。
//! 空串与纯空白一律按未设置处理。默认 `.env` 路径取编译期 `CARGO_MANIFEST_DIR`
//! 下的 `.env`,不受进程当前目录影响。
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// heavy 门槛开关:必须恰为 `1`(见 [`require_heavy`])。
pub const HEAVY: &str = "MNEME_HEAVY";
/// heavy 规模(行数);缺省见 `common::HEAVY_DEFAULT_ROWS`。
pub const HEAVY_ROWS: &str = "MNEME_HEAVY_ROWS";
/// heavy 向量维度;缺省见 `common::HEAVY_DEFAULT_DIMENSION`。
pub const HEAVY_DIM: &str = "MNEME_HEAVY_DIM";
/// 建库块级并行度(测试读入后注入 `Tuning::flush_threads`)。
pub const FLUSH_THREADS: &str = "MNEME_FLUSH_THREADS";
/// 建库切块行数(测试读入后注入 `Tuning::flush_chunk_rows`)。
pub const FLUSH_CHUNK_ROWS: &str = "MNEME_FLUSH_CHUNK_ROWS";
/// 加密 fixture 目录(必填,见 `tests/security_contracts.rs`)。
pub const ENCRYPT_FIXTURE: &str = "MNEME_ENCRYPT_FIXTURE";
/// f16 fixture 目录(必填,见 `tests/l6_contracts.rs`)。
pub const F16_FIXTURE: &str = "MNEME_F16_FIXTURE";
/// 示例嵌入 API key(优先;见 `examples/memory/embedding.rs`)。
pub const EXAMPLE_API_KEY: &str = "EXAMPLE_EMBEDDING_API_KEY";
/// 示例嵌入 API key 兜底(OpenAI 官方变量名)。
pub const OPENAI_API_KEY: &str = "OPENAI_API_KEY";
/// 示例嵌入端点(优先)。
pub const EMBEDDING_BASE_URL: &str = "MNEME_EMBEDDING_BASE_URL";
/// 示例嵌入端点兜底(OpenAI 官方变量名)。
pub const OPENAI_BASE_URL: &str = "OPENAI_BASE_URL";
/// 示例嵌入模型。
pub const EMBEDDING_MODEL: &str = "MNEME_EMBEDDING_MODEL";
/// 示例库目录。
pub const EXAMPLE_PATH: &str = "MNEME_EXAMPLE_PATH";

/// `.env` 文件 + 进程环境的合并读取器(dev 目标共用)。
///
/// 进程环境优先;`.env` 只作兜底,不覆盖真实环境变量。只认 `KEY=VALUE` 行,
/// 忽略空行、`#` 注释与不含 `=` 的行;`KEY = "VALUE"` 这种带空白与成对引号的
/// 写法也接受(示例/测试共用同一解析口径,不引 dotenv crate)。
pub struct EnvFile {
    /// `.env` 解析出的键值表。
    entries: HashMap<String, String>,
    /// 是否把真实进程环境并入读取(测试可用 [`EnvFile::entries_only`] 关闭)。
    include_process: bool,
}

impl EnvFile {
    /// 读取给定路径的 `.env`;文件不存在或不可读时按空表处理。
    ///
    /// # Arguments
    /// * `path` - `.env` 路径;示例传 [`EnvFile::repo`] 定位的仓库根 `.env`。
    ///
    /// # Returns
    /// 环境变量表;与真实环境变量的合并规则见 [`EnvFile::get`]。
    pub fn load(path: impl AsRef<Path>) -> Self {
        let entries = std::fs::read_to_string(path)
            .map(|raw| parse_dotenv(&raw))
            .unwrap_or_default();
        Self {
            entries,
            include_process: true,
        }
    }

    /// 仓库根 `.env`(惰性加载,进程内唯一;路径见 [`repo_dotenv_path`])。
    ///
    /// 测试与示例默认经此读取,支持把常用变量(heavy 开关/规模、嵌入配置等)
    /// 写进 `.env` 而无需每次 export。
    pub fn repo() -> &'static Self {
        static FILE: OnceLock<EnvFile> = OnceLock::new();
        FILE.get_or_init(|| EnvFile::load(repo_dotenv_path()))
    }

    /// 取变量:真实环境变量 > `.env`;空串与纯空白按未设置处理。
    ///
    /// # Arguments
    /// * `key` - 变量名。
    ///
    /// # Returns
    /// 变量值;两处都未设置时返回 `None`。
    pub fn get(&self, key: &str) -> Option<String> {
        if self.include_process
            && let Some(from_process) = process_get(key)
        {
            return Some(from_process);
        }
        self.entries
            .get(key)
            .filter(|value| !value.trim().is_empty())
            .cloned()
    }
}

#[cfg(test)]
impl EnvFile {
    /// 测试用构造:只查给定条目,完全不读进程环境与 `.env`(用例不污染、不受环境干扰)。
    ///
    /// # Arguments
    /// * `entries` - `(键, 值)` 列表。
    pub fn entries_only(entries: &[(&str, &str)]) -> Self {
        Self {
            entries: entries
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            include_process: false,
        }
    }
}

/// 仓库根 `.env` 路径(编译期 `CARGO_MANIFEST_DIR` 定位,不受 cwd 影响)。
pub fn repo_dotenv_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(".env")
}

/// 解析 `.env` 文本为键值表;忽略注释、空行与不含 `=` 的行。
fn parse_dotenv(raw: &str) -> HashMap<String, String> {
    raw.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            let value = value.trim().trim_matches(['"', '\'']);
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

/// 只读真实进程环境(空串与纯空白按未设置处理)。
pub fn process_get(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// 读取变量:真实环境变量 > 仓库根 `.env`;未设置、空串或纯空白按未设置处理。
///
/// # Arguments
/// * `name` - 变量名(用本模块常量,不散写字面量)。
///
/// # Returns
/// 去首尾空白后的变量值;两处都未设置时返回 `None`。
pub fn get(name: &str) -> Option<String> {
    EnvFile::repo().get(name)
}

/// 布尔开关:仅值恰为 `1` 视为开启(避免 `true`/`yes` 等多口径)。
pub fn flag(name: &str) -> bool {
    get(name).as_deref() == Some("1")
}

/// 必填变量:缺失时显式 panic(报错只含变量名,绝不回显值)。
///
/// # Panics
/// 变量在真实环境与 `.env` 中都未设置或为空值时 panic。
pub fn require(name: &str) -> String {
    get(name)
        .unwrap_or_else(|| panic!("环境变量 {name} 未设置(或为空);见 tests/common/env.rs 变量清单"))
}

/// 读取正整数:未设置用 `default`;配错不得静默回退。
///
/// # Panics
/// 值无法解析为 `usize` 时 panic。
pub fn usize_or(name: &str, default: usize) -> usize {
    match get(name) {
        Some(value) => value
            .parse()
            .unwrap_or_else(|_| panic!("{name} 非法(需为正整数): {value:?}")),
        None => default,
    }
}

/// 读取可选正整数:未设置返回 `None`;配错不得静默回退。
///
/// # Panics
/// 值无法解析为 `usize` 时 panic。
pub fn optional_usize(name: &str) -> Option<usize> {
    get(name).map(|value| {
        value
            .parse()
            .unwrap_or_else(|_| panic!("{name} 非法(需为正整数): {value:?}"))
    })
}

/// 断言 heavy 门槛开关已打开([`HEAVY`] 恰为 `1`)。
///
/// heavy 用例默认 `#[ignore]`;本函数防「CI 配错 env 却静默跳过」的假绿。
///
/// # Panics
/// [`HEAVY`] 未设置为 `1` 时 panic。
pub fn require_heavy() {
    if !flag(HEAVY) {
        panic!("{HEAVY}=1 未设置:heavy 门槛不应静默跳过(显式失败,杜绝 CI 假绿)");
    }
}
