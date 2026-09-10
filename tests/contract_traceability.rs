//! 契约追溯门禁(FSVDD §10,替代原计划的 `xtask check-contracts`)。
//!
//! 在 `cargo test` 中机械校验 `docs/spec/contracts.md` 与测试的双向映射:
//!
//! 1. 契约引用的每个 `<path>.rs::<test>` 必须真实存在,且测试名必须定义在该路径
//!    指向的文件中(路径级匹配,无悬空引用、无跨文件误配、无未纳管路径);
//! 2. 各契约测试文件中的每个 `#[test]` 必须被至少一条契约引用(无孤立测试);
//! 3. 状态列为 `Passed` 的契约条目必须登记至少一个真实测试路径(禁止
//!    「已通过却无测试证明」的自由文本);
//! 4. 各契约测试文件 doc 注释声明的 `FC-*` 编号(展开 `/002` 复用与 `001..003`
//!    区间简写)必须与契约矩阵中引用该文件的条目**双向相等**(无多报、无少报)。
//!
//! 说明:源码内的单元测试(操作计数 / 公式)可被契约引用,但不强制每个单元测试
//! 都登记 FC 编号——它们是实现细节测试,契约门禁只对集成契约测试文件做孤立与
//! 声明一致性检查。L1 契约测试按 FC 模块族拆分(memory/query/model/life),门禁
//! 统一纳入。

use std::collections::{BTreeMap, HashSet};

/// 形式化契约矩阵(唯一真实数据源)。
const CONTRACTS: &str = include_str!("../docs/spec/contracts.md");
/// L0 契约验收测试。
const CORE_TESTS: &str = include_str!("core_contracts.rs");
/// L1 契约验收测试(写入/更新/删除/去重/限额)。
const MEMORY_TESTS: &str = include_str!("memory_contracts.rs");
/// L1 契约验收测试(检索/过滤/打分/反馈)。
const QUERY_TESTS: &str = include_str!("query_contracts.rs");
/// L1 契约验收测试(关系/双时态/版本状态机/沉淀)。
const MODEL_TESTS: &str = include_str!("model_contracts.rs");
/// L1 契约验收测试(遗忘/库生命周期/错误分类)。
const LIFE_TESTS: &str = include_str!("life_contracts.rs");
/// L2 契约验收测试(持久化/崩溃恢复/覆盖持久性)。
const PERSIST_TESTS: &str = include_str!("persist_contracts.rs");
/// 承载操作计数单测的源码文件。
const SRC_SEARCH: &str = include_str!("../src/memory/search.rs");
const SRC_TABLE: &str = include_str!("../src/memory/table/mod.rs");
const SRC_LIFECYCLE: &str = include_str!("../src/memory/lifecycle.rs");
/// L2 编解码源码(片级损坏/版本拒绝单测被契约引用)。
const SRC_VSEC: &str = include_str!("../src/persist/vsec.rs");
const SRC_MSEC: &str = include_str!("../src/persist/msec/mod.rs");
const SRC_WAL: &str = include_str!("../src/persist/wal/mod.rs");
const SRC_MANIFEST: &str = include_str!("../src/persist/manifest.rs");

/// 契约测试文件(孤立检查与覆盖声明检查的范围)。
const CONTRACT_TEST_FILES: [(&str, &str); 6] = [
    ("tests/core_contracts.rs", CORE_TESTS),
    ("tests/memory_contracts.rs", MEMORY_TESTS),
    ("tests/query_contracts.rs", QUERY_TESTS),
    ("tests/model_contracts.rs", MODEL_TESTS),
    ("tests/life_contracts.rs", LIFE_TESTS),
    ("tests/persist_contracts.rs", PERSIST_TESTS),
];

/// 契约引用的测试可能落在的全部文件(路径必须与 `contracts.md` 中书写一致)。
const SOURCES: [(&str, &str); 13] = [
    ("tests/core_contracts.rs", CORE_TESTS),
    ("tests/memory_contracts.rs", MEMORY_TESTS),
    ("tests/query_contracts.rs", QUERY_TESTS),
    ("tests/model_contracts.rs", MODEL_TESTS),
    ("tests/life_contracts.rs", LIFE_TESTS),
    ("tests/persist_contracts.rs", PERSIST_TESTS),
    ("src/memory/search.rs", SRC_SEARCH),
    ("src/memory/table/mod.rs", SRC_TABLE),
    ("src/memory/lifecycle.rs", SRC_LIFECYCLE),
    ("src/persist/vsec.rs", SRC_VSEC),
    ("src/persist/msec/mod.rs", SRC_MSEC),
    ("src/persist/wal/mod.rs", SRC_WAL),
    ("src/persist/manifest.rs", SRC_MANIFEST),
];

/// 契约编号的类型段(五维 + CPLX,见 `contracts.md` §0)。
const FC_KINDS: [&str; 6] = ["PRE", "POST", "INV", "STA", "ERR", "CPLX"];

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

/// 读取十进制数字串,返回(值, 消费字节数)。
fn read_num(bytes: &[u8]) -> Option<(u32, usize)> {
    let mut cursor = 0;
    while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
        cursor += 1;
    }
    if cursor == 0 {
        return None;
    }
    let value = std::str::from_utf8(&bytes[..cursor]).ok()?.parse().ok()?;
    Some((value, cursor))
}

/// 在 `bytes[start..]` 处解析 `FC-<模块>-<类型>-<序号>`(`"FC-"` 前缀由调用方保证),
/// 返回 (模块, 类型, 序号, 消费的总字节数);不属于完整契约编号的文本返回 `None`。
fn parse_fc_at(bytes: &[u8], start: usize) -> Option<(String, String, u32, usize)> {
    let mut cursor = start + 3;
    let module_start = cursor;
    while cursor < bytes.len() && bytes[cursor].is_ascii_alphanumeric() {
        cursor += 1;
    }
    if cursor == module_start || cursor >= bytes.len() || bytes[cursor] != b'-' {
        return None;
    }
    let module = String::from_utf8_lossy(&bytes[module_start..cursor]).to_string();
    cursor += 1;
    let kind_start = cursor;
    while cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
        cursor += 1;
    }
    let kind = String::from_utf8_lossy(&bytes[kind_start..cursor]).to_string();
    if !FC_KINDS.contains(&kind.as_str()) {
        return None;
    }
    if cursor >= bytes.len() || bytes[cursor] != b'-' {
        return None;
    }
    cursor += 1;
    let (first, num_len) = read_num(&bytes[cursor..])?;
    cursor += num_len;
    Some((module, kind, first, cursor - start))
}

/// 从文本提取全部 `FC-*` 编号,展开 `/002` 复用与 `001..003` 区间简写,去重排序。
fn fc_ids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut ids = Vec::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        if &bytes[index..index + 3] != b"FC-" {
            index += 1;
            continue;
        }
        let Some((module, kind, first, mut consumed)) = parse_fc_at(bytes, index) else {
            index += 3;
            continue;
        };
        ids.push(format!("FC-{module}-{kind}-{first:03}"));
        // 同族简写尾巴:`/002`、`..003`,可交替(如 `001..003/005`)。
        loop {
            let rest = &bytes[index + consumed..];
            if rest.first() == Some(&b'/') {
                let Some((num, len)) = read_num(&rest[1..]) else {
                    break;
                };
                ids.push(format!("FC-{module}-{kind}-{num:03}"));
                consumed += 1 + len;
            } else if rest.starts_with(b"..") {
                let Some((last, len)) = read_num(&rest[2..]) else {
                    break;
                };
                for value in (first + 1)..=last {
                    ids.push(format!("FC-{module}-{kind}-{value:03}"));
                }
                consumed += 2 + len;
            } else {
                break;
            }
        }
        index += consumed;
    }
    ids.sort();
    ids.dedup();
    ids
}

/// 解析契约矩阵条目行 `| FC-... | 类型 | 规范 | 对应测试 | 状态 |`,
/// 返回 (编号, 对应测试列, 状态列)。
fn contract_rows() -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    for line in CONTRACTS.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("| FC-") {
            continue;
        }
        // 规范列的 KaTeX 公式含 `\|`(转义竖线),先替换占位再按列切分。
        let normalized = trimmed.replace("\\|", "\u{0}");
        let cells: Vec<&str> = normalized.split('|').collect();
        // cells[0] 为行首管道前的空串;编号 / 测试 / 状态分别在 1 / 4 / 5。
        if cells.len() < 6 {
            continue;
        }
        rows.push((
            cells[1].trim().to_string(),
            cells[4].trim().replace('\u{0}', "\\|"),
            cells[5].trim().to_string(),
        ));
    }
    rows
}

/// 提取 `contracts.md` 中所有 `<path>.rs::<name>` 引用(路径级,去重排序)。
fn referenced_test_refs() -> Vec<(String, String)> {
    let bytes = CONTRACTS.as_bytes();
    let needle = b".rs::";
    let mut refs = Vec::new();
    let mut index = 0;
    while index + needle.len() <= bytes.len() {
        if &bytes[index..index + needle.len()] != needle {
            index += 1;
            continue;
        }
        // 名称:needle 之后直到首个非 [A-Za-z0-9_] 字符。
        let name_start = index + needle.len();
        let mut cursor = name_start;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
        {
            cursor += 1;
        }
        // 路径:从 `.rs` 向前收集路径字符(字母数字 `/ _ -`),直到反引号等锚点。
        let mut start = index;
        while start > 0 {
            let previous = bytes[start - 1];
            if previous.is_ascii_alphanumeric()
                || previous == b'/'
                || previous == b'_'
                || previous == b'-'
            {
                start -= 1;
            } else {
                break;
            }
        }
        if cursor > name_start {
            let path = String::from_utf8_lossy(&bytes[start..index + 3]).to_string();
            refs.push((
                path,
                String::from_utf8_lossy(&bytes[name_start..cursor]).to_string(),
            ));
        }
        index = cursor.max(index + needle.len());
    }
    refs.sort();
    refs.dedup();
    refs
}

/// 契约引用的每个测试都必须存在于其路径指向的文件中(路径级匹配)。
#[test]
fn every_referenced_test_exists() {
    let defined: BTreeMap<&str, Vec<String>> = SOURCES
        .iter()
        .map(|(path, source)| (*path, test_fns(source)))
        .collect();
    let dangling: Vec<String> = referenced_test_refs()
        .into_iter()
        .filter(|(path, name)| {
            !defined
                .get(path.as_str())
                .is_some_and(|tests| tests.contains(name))
        })
        .map(|(path, name)| format!("{path}::{name}"))
        .collect();
    assert!(
        dangling.is_empty(),
        "契约引用了不存在的测试(悬空引用或未纳管路径): {dangling:?}"
    );
}

/// 契约测试文件中的每个测试都必须可追溯到某条 FC。
#[test]
fn no_orphan_contract_tests() {
    let referenced: HashSet<String> = referenced_test_refs()
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    let orphans: Vec<String> = CONTRACT_TEST_FILES
        .iter()
        .flat_map(|(_, source)| test_fns(source))
        .filter(|name| !referenced.contains(name))
        .collect();
    assert!(
        orphans.is_empty(),
        "存在未映射到任何 FC 的孤立测试: {orphans:?}"
    );
}

/// 状态列为 `Passed` 的条目必须登记真实测试路径。
#[test]
fn passed_contracts_register_real_tests() {
    let unproven: Vec<String> = contract_rows()
        .into_iter()
        .filter(|(_, tests, status)| status == "Passed" && !tests.contains(".rs::"))
        .map(|(id, tests, _)| format!("{id}: 对应测试 = {tests:?}"))
        .collect();
    assert!(
        unproven.is_empty(),
        "Passed 条目未登记任何测试路径(无测试证明): {unproven:?}"
    );
}

/// `contracts.md` 中「对应测试」列引用了某测试文件的条目编号集合。
fn coverage_by_file() -> BTreeMap<&'static str, Vec<String>> {
    let mut map: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for (id, tests, _) in contract_rows() {
        for (path, _) in CONTRACT_TEST_FILES {
            if tests.contains(&format!("{path}::")) {
                map.entry(path).or_default().push(id.clone());
            }
        }
    }
    map
}

/// 测试文件 doc 注释声明的 FC 条目 ↔ 契约覆盖该文件的条目,双向相等。
#[test]
fn declared_fcs_match_contract_coverage() {
    let coverage = coverage_by_file();
    for (path, source) in CONTRACT_TEST_FILES {
        let declared = fc_ids(source);
        let mut covered = coverage.get(path).cloned().unwrap_or_default();
        covered.sort();
        covered.dedup();
        let over: Vec<&String> = declared.iter().filter(|id| !covered.contains(id)).collect();
        let under: Vec<&String> = covered.iter().filter(|id| !declared.contains(id)).collect();
        assert!(
            over.is_empty() && under.is_empty(),
            "{path} 的 FC 覆盖声明与 contracts.md 不一致——多报: {over:?} 少报: {under:?}"
        );
    }
}
