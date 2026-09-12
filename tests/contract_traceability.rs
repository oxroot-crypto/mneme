//! 契约追溯门禁(FSVDD §10,替代原计划的 `xtask check-contracts`)。
//!
//! 在 `cargo test` 中机械校验 `docs/spec/contracts.md` 与测试的双向映射:
//!
//! 1. 契约引用的每个 `<path>.rs::<test>` 必须真实存在,且测试名必须定义在该路径
//!    指向的文件中(路径级匹配,无悬空引用、无跨文件误配、无未纳管路径);
//! 2. 各契约测试文件中的每个 `#[test]` 必须在契约矩阵「对应测试」列中被引用
//!    (无孤立测试;只看表格行,防止正文随意提及放水);
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
/// L3 契约验收测试(HNSW 召回/收敛/过滤/hidx)。
const HNSW_TESTS: &str = include_str!("hnsw_contracts.rs");
/// L4 契约验收测试(DSL/BM25/融合/计划器/四区落盘)。
const L4_TESTS: &str = include_str!("l4_contracts.rs");
/// L5 契约验收测试(多段增量 flush/delta/WAL 轮转/多图检索)。
const L5_TESTS: &str = include_str!("l5_contracts.rs");
/// 承载操作计数单测的源码文件。
const SRC_SEARCH: &str = include_str!("../src/memory/search.rs");
const SRC_TABLE: &str = include_str!("../src/memory/table/state.rs");
const SRC_LIFECYCLE: &str = include_str!("../src/memory/lifecycle.rs");
/// L1 双时态视图源码(`snapshot_at` 保留索引句柄单测被契约引用)。
const SRC_TEMPORAL: &str = include_str!("../src/memory/temporal.rs");
/// L3 索引源码(HNSW 操作计数与图不变量单测被契约引用)。
const SRC_INDEX_HNSW: &str = include_str!("../src/index/hnsw.rs");
const SRC_INDEX_HIDX: &str = include_str!("../src/index/hidx.rs");
/// L3 过滤三档源码(档位选择单测被契约引用)。
const SRC_INDEX_FILTERED: &str = include_str!("../src/index/filtered.rs");
/// L2 编解码源码(片级损坏/版本拒绝单测被契约引用)。
const SRC_VSEC: &str = include_str!("../src/persist/vsec.rs");
const SRC_MSEC: &str = include_str!("../src/persist/msec/mod.rs");
const SRC_WAL: &str = include_str!("../src/persist/wal/mod.rs");
const SRC_MANIFEST: &str = include_str!("../src/persist/manifest.rs");
/// L2 恢复/锁源码(批原子校验、独占锁语义单测被契约引用)。
const SRC_RECOVER_REPLAY: &str = include_str!("../src/persist/recover/replay.rs");
const SRC_STORAGE: &str = include_str!("../src/persist/storage.rs");
/// 段读取后端源码(`read_whole`/`MmapSource` 单测被契约引用)。
const SRC_SOURCE: &str = include_str!("../src/persist/source.rs");
/// L0 SIMD 源码(逐元素乘加计数单测被 CPLX 契约引用)。
const SRC_CORE_SIMD: &str = include_str!("../src/core/simd.rs");
/// L0 TopK 源码(堆操作计数单测被 CPLX 契约引用)。
const SRC_CORE_HEAP: &str = include_str!("../src/core/heap.rs");
/// L2 恢复重排映射源码(槽位一致性单测被 ERR 契约引用)。
const SRC_RECOVER_STATE: &str = include_str!("../src/persist/recover/state.rs");
/// L2 打开路径源码(载入期重排越界二次校验单测被 ERR 契约引用)。
const SRC_STORE_OPEN: &str = include_str!("../src/persist/store/open.rs");
/// L4 计划器源码(块级剪枝等价性单测被契约引用)。
const SRC_QUERY_PLAN: &str = include_str!("../src/query/plan.rs");
/// L4 解析器源码(越界/位置错误单测被契约引用)。
const SRC_QUERY_PARSE: &str = include_str!("../src/query/parse.rs");
/// L4 BM25 源码(公式/可见性单测被契约引用)。
const SRC_QUERY_BM25: &str = include_str!("../src/query/bm25.rs");
/// L4 融合源码(RRF/加权单测被契约引用)。
const SRC_QUERY_FUSION: &str = include_str!("../src/query/fusion.rs");
/// L2 倒排区源码(畸形结构拒绝单测被契约引用)。
const SRC_MSEC_INVERTED: &str = include_str!("../src/persist/msec/inverted.rs");
const SRC_MSEC_INDEX: &str = include_str!("../src/persist/msec/index.rs");
/// L1 bloom 源码(极值 fpp 夹紧单测被 CPLX 契约引用)。
const SRC_ANALYSIS_BLOOM: &str = include_str!("../src/memory/analysis/bloom.rs");
/// L1 zone map 源码(类型冲突退出剪枝单测被查询契约引用)。
const SRC_ANALYSIS_ZONES: &str = include_str!("../src/memory/analysis/zones.rs");
/// L4 打印/JSON 往返源码(空列表规约单测被 POST 契约引用)。
const SRC_QUERY_DISPLAY: &str = include_str!("../src/query/display.rs");
const SRC_QUERY_JSON: &str = include_str!("../src/query/json.rs");
/// L0 分词源码(切词/bigram/停用词单测被 POST 契约引用)。
const SRC_CORE_TEXT: &str = include_str!("../src/core/text.rs");
/// L4 ISO 8601 源码(毫秒补零/往返单测被 POST 契约引用)。
const SRC_QUERY_ISO: &str = include_str!("../src/query/iso.rs");
/// L1 内存倒排源码(NS 隔离/词频累计单测被 INV 契约引用)。
const SRC_ANALYSIS_INV: &str = include_str!("../src/memory/analysis/inv.rs");
/// L2 flush 段编码源码(字段字典 `key` 去重单测被 POST 契约引用)。
const SRC_PERSIST_FLUSH: &str = include_str!("../src/persist/flush.rs");
/// L2 msec delta 区源码(往返/畸形拒绝单测被 POST/ERR 契约引用)。
const SRC_MSEC_DELTA: &str = include_str!("../src/persist/msec/delta.rs");
/// L2 msec 记录体源码(字段标志畸形拒绝单测被 ERR 契约引用)。
const SRC_MSEC_ENTRY: &str = include_str!("../src/persist/msec/entry.rs");
/// L2 WAL 写入器源码(文件序号解析单测被 POST 契约引用)。
const SRC_WAL_WRITER: &str = include_str!("../src/persist/store/wal_writer.rs");
/// L5 调度源码(选段/幸存筛选单测被 CPLX 契约引用)。
const SRC_LIFE_COMPACT: &str = include_str!("../src/life/compact.rs");
/// L2 关系区源码(正/反向表往返单测被 POST 契约引用)。
const SRC_PERSIST_EDGES: &str = include_str!("../src/persist/edges.rs");
/// L1 谓词求值源码(保留字段清单同步单测被 POST 契约引用)。
const SRC_PRED_EVAL: &str = include_str!("../src/memory/pred_eval.rs");
/// L5 运维源码(compaction 状态机转移单测被 STA 契约引用)。
const SRC_MEMORY_OPS: &str = include_str!("../src/memory/ops.rs");
/// L2 增量 flush/备份源码(硬链接回退单测被 POST 契约引用)。
const SRC_STORE_SNAPSHOT: &str = include_str!("../src/persist/store/snapshot.rs");

/// 契约测试文件(孤立检查与覆盖声明检查的范围)。
const CONTRACT_TEST_FILES: [(&str, &str); 9] = [
    ("tests/core_contracts.rs", CORE_TESTS),
    ("tests/memory_contracts.rs", MEMORY_TESTS),
    ("tests/query_contracts.rs", QUERY_TESTS),
    ("tests/model_contracts.rs", MODEL_TESTS),
    ("tests/life_contracts.rs", LIFE_TESTS),
    ("tests/persist_contracts.rs", PERSIST_TESTS),
    ("tests/hnsw_contracts.rs", HNSW_TESTS),
    ("tests/l4_contracts.rs", L4_TESTS),
    ("tests/l5_contracts.rs", L5_TESTS),
];

/// 契约引用的测试可能落在的全部文件(路径必须与 `contracts.md` 中书写一致)。
const SOURCES: [(&str, &str); 49] = [
    ("tests/core_contracts.rs", CORE_TESTS),
    ("tests/memory_contracts.rs", MEMORY_TESTS),
    ("tests/query_contracts.rs", QUERY_TESTS),
    ("tests/model_contracts.rs", MODEL_TESTS),
    ("tests/life_contracts.rs", LIFE_TESTS),
    ("tests/persist_contracts.rs", PERSIST_TESTS),
    ("tests/hnsw_contracts.rs", HNSW_TESTS),
    ("tests/l4_contracts.rs", L4_TESTS),
    ("tests/l5_contracts.rs", L5_TESTS),
    ("src/memory/search.rs", SRC_SEARCH),
    ("src/memory/table/state.rs", SRC_TABLE),
    ("src/memory/lifecycle.rs", SRC_LIFECYCLE),
    ("src/memory/temporal.rs", SRC_TEMPORAL),
    ("src/index/hnsw.rs", SRC_INDEX_HNSW),
    ("src/index/hidx.rs", SRC_INDEX_HIDX),
    ("src/index/filtered.rs", SRC_INDEX_FILTERED),
    ("src/persist/vsec.rs", SRC_VSEC),
    ("src/persist/msec/mod.rs", SRC_MSEC),
    ("src/persist/wal/mod.rs", SRC_WAL),
    ("src/persist/manifest.rs", SRC_MANIFEST),
    ("src/persist/recover/replay.rs", SRC_RECOVER_REPLAY),
    ("src/persist/storage.rs", SRC_STORAGE),
    ("src/persist/source.rs", SRC_SOURCE),
    ("src/core/simd.rs", SRC_CORE_SIMD),
    ("src/core/heap.rs", SRC_CORE_HEAP),
    ("src/persist/recover/state.rs", SRC_RECOVER_STATE),
    ("src/persist/store/open.rs", SRC_STORE_OPEN),
    ("src/query/plan.rs", SRC_QUERY_PLAN),
    ("src/query/parse.rs", SRC_QUERY_PARSE),
    ("src/query/bm25.rs", SRC_QUERY_BM25),
    ("src/query/fusion.rs", SRC_QUERY_FUSION),
    ("src/persist/msec/inverted.rs", SRC_MSEC_INVERTED),
    ("src/persist/msec/index.rs", SRC_MSEC_INDEX),
    ("src/memory/analysis/bloom.rs", SRC_ANALYSIS_BLOOM),
    ("src/memory/analysis/zones.rs", SRC_ANALYSIS_ZONES),
    ("src/query/display.rs", SRC_QUERY_DISPLAY),
    ("src/query/json.rs", SRC_QUERY_JSON),
    ("src/core/text.rs", SRC_CORE_TEXT),
    ("src/query/iso.rs", SRC_QUERY_ISO),
    ("src/memory/analysis/inv.rs", SRC_ANALYSIS_INV),
    ("src/persist/flush.rs", SRC_PERSIST_FLUSH),
    ("src/memory/pred_eval.rs", SRC_PRED_EVAL),
    ("src/persist/msec/delta.rs", SRC_MSEC_DELTA),
    ("src/persist/msec/entry.rs", SRC_MSEC_ENTRY),
    ("src/persist/store/wal_writer.rs", SRC_WAL_WRITER),
    ("src/life/compact.rs", SRC_LIFE_COMPACT),
    ("src/persist/edges.rs", SRC_PERSIST_EDGES),
    ("src/memory/ops.rs", SRC_MEMORY_OPS),
    ("src/persist/store/snapshot.rs", SRC_STORE_SNAPSHOT),
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
        if !pending {
            continue;
        }
        // 跳过 `#[test]` 与声明之间可能出现的注释/其它属性行,
        // 防止 `// fn foo` 之类注释被误当成测试名(也会让真孤立测试逃逸)。
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("#[") {
            continue;
        }
        if let Some(name) = fn_name(trimmed) {
            names.push(name);
        }
        pending = false;
    }
    names
}

/// 从形如 `fn foo(...)` 的行提取 `foo`;支持 `pub` / `async` / `pub(crate)` 等前缀。
fn fn_name(line: &str) -> Option<String> {
    let rest = line.rsplit_once("fn ")?.1;
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
/// 返回 (编号, 对应测试列, 状态列)。编号列允许带反引号包裹。
fn contract_rows() -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    for line in CONTRACTS.lines() {
        let trimmed = line.trim();
        // 规范列的 KaTeX 公式含 `\|`(转义竖线),先替换占位再按列切分。
        let normalized = trimmed.replace("\\|", "\u{0}");
        let cells: Vec<&str> = normalized.split('|').collect();
        // cells[0] 为行首管道前的空串;编号 / 测试 / 状态分别在 1 / 4 / 5。
        if cells.len() < 6 {
            continue;
        }
        let id = cells[1].trim().trim_matches('`');
        if !id.starts_with("FC-") {
            continue;
        }
        rows.push((
            id.to_string(),
            cells[4].trim().replace('\u{0}', "\\|"),
            cells[5].trim().to_string(),
        ));
    }
    rows
}

/// 提取文本中所有 `<path>.rs::<name>` 引用(路径级,去重排序)。
fn referenced_test_refs(text: &str) -> Vec<(String, String)> {
    let bytes = text.as_bytes();
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

/// 提取契约矩阵**「对应测试」列**中的全部引用(孤立测试判定口径)。
///
/// 只认表格行,不认变更记录或说明文字——否则正文里随便提一句测试名
/// 就能让一个无 FC 映射的测试免于被检出。
fn row_test_refs() -> Vec<(String, String)> {
    let mut refs: Vec<(String, String)> = contract_rows()
        .into_iter()
        .flat_map(|(_, tests, _)| referenced_test_refs(&tests))
        .collect();
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
    let dangling: Vec<String> = referenced_test_refs(CONTRACTS)
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

/// 契约测试文件中的每个测试都必须可追溯到某条 FC(按路径 + 测试名配对,
/// 防止跨文件同名测试互相顶替)。
#[test]
fn no_orphan_contract_tests() {
    let referenced: HashSet<(String, String)> = row_test_refs().into_iter().collect();
    let orphans: Vec<String> = CONTRACT_TEST_FILES
        .iter()
        .flat_map(|(path, source)| {
            test_fns(source).into_iter().filter_map(|name| {
                let key = ((*path).to_string(), name);
                (!referenced.contains(&key)).then(|| format!("{}::{}", key.0, key.1))
            })
        })
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
        .filter(|(_, tests, status)| status == "Passed" && referenced_test_refs(tests).is_empty())
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

/// `tests/` 下每个 `*_contracts.rs` 都必须登记进门禁清单,防新文件逃逸出追溯。
#[test]
fn every_contract_test_file_is_registered() {
    let registered: HashSet<String> = CONTRACT_TEST_FILES
        .iter()
        .map(|(path, _)| path.strip_prefix("tests/").unwrap_or(*path).to_string())
        .collect();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
    let mut unregistered = Vec::new();
    for entry in std::fs::read_dir(dir).expect("读取 tests 目录") {
        let name = entry
            .expect("目录项")
            .file_name()
            .to_string_lossy()
            .to_string();
        if name.ends_with("_contracts.rs") && !registered.contains(&name) {
            unregistered.push(name);
        }
    }
    unregistered.sort();
    assert!(
        unregistered.is_empty(),
        "tests/ 下存在未登记进 CONTRACT_TEST_FILES 的契约测试文件: {unregistered:?}"
    );
}
