//! 契约追溯门禁(FSVDD §10,替代原计划的 `xtask check-contracts`)。
//!
//! 在 `cargo test` 中机械校验 `docs/spec/contracts.md` 与测试的双向映射:
//!
//! 1. 每个契约引用的 `*.rs::<test>` 必须在源码中真实存在(无悬空引用);
//! 2. 两个契约测试文件中的每个 `#[test]` 必须被至少一条契约引用(无孤立测试)。
//!
//! 说明:源码内的单元测试(操作计数 / 公式)可被契约引用,但不强制每个单元测试
//! 都登记 FC 编号——它们是实现细节测试,契约门禁只对集成契约测试文件做孤立检查。

use std::collections::HashSet;

/// 形式化契约矩阵(唯一真实数据源)。
const CONTRACTS: &str = include_str!("../docs/spec/contracts.md");
/// L0 契约验收测试。
const CORE_TESTS: &str = include_str!("core_contracts.rs");
/// L1 契约验收测试。
const MEMORY_TESTS: &str = include_str!("memory_contracts.rs");
/// 承载操作计数单测的源码文件。
const SRC_SEARCH: &str = include_str!("../src/memory/search.rs");
const SRC_TABLE: &str = include_str!("../src/memory/table.rs");
const SRC_LIFECYCLE: &str = include_str!("../src/memory/lifecycle.rs");

/// 提取源码中所有 `#[test]` 之后的函数名。
fn test_fns(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut pending = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[test]") {
            pending = true;
            continue;
        }
        if pending && let Some(name) = fn_name(trimmed) {
            names.push(name);
            pending = false;
        }
    }
    names
}

/// 从形如 `fn foo(...)` 的行提取 `foo`。
fn fn_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("fn ")?;
    let end = rest.find(['(', '<', ' ']).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// 提取 `contracts.md` 中所有 `*.rs::<name>` 引用的测试名。
fn referenced_tests() -> Vec<String> {
    let mut names = Vec::new();
    let bytes = CONTRACTS.as_bytes();
    let needle = b".rs::";
    let mut index = 0;
    while index + needle.len() < bytes.len() {
        if &bytes[index..index + needle.len()] == needle {
            let mut cursor = index + needle.len();
            let start = cursor;
            while cursor < bytes.len()
                && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
            {
                cursor += 1;
            }
            if cursor > start {
                names.push(String::from_utf8_lossy(&bytes[start..cursor]).to_string());
            }
            index = cursor;
        } else {
            index += 1;
        }
    }
    names.sort();
    names.dedup();
    names
}

/// 契约引用的每个测试都必须真实存在。
#[test]
fn every_referenced_test_exists() {
    let mut defined: HashSet<String> = HashSet::new();
    defined.extend(test_fns(CORE_TESTS));
    defined.extend(test_fns(MEMORY_TESTS));
    defined.extend(test_fns(SRC_SEARCH));
    defined.extend(test_fns(SRC_TABLE));
    defined.extend(test_fns(SRC_LIFECYCLE));

    let missing: Vec<String> = referenced_tests()
        .into_iter()
        .filter(|name| !defined.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "契约引用了不存在的测试(悬空引用): {missing:?}"
    );
}

/// 契约测试文件中的每个测试都必须可追溯到某条 FC。
#[test]
fn no_orphan_contract_tests() {
    let referenced: HashSet<String> = referenced_tests().into_iter().collect();
    let orphans: Vec<String> = test_fns(CORE_TESTS)
        .into_iter()
        .chain(test_fns(MEMORY_TESTS))
        .filter(|name| !referenced.contains(name))
        .collect();
    assert!(
        orphans.is_empty(),
        "存在未映射到任何 FC 的孤立测试: {orphans:?}"
    );
}
