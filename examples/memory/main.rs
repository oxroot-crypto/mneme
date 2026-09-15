//! 端到端示例:交互式记忆库,经第三方嵌入 API(OpenAI 协议)读写 Mneme。
//!
//! 数据流(库本体零网络,嵌入在宿主侧,见 `docs/design/16-api-reference.md` §6):
//!
//! ```text
//! /add <文本>     ──嵌入──▶ 向量 ──insert──▶ Mneme(段/WAL)
//! /search <文本>  ──嵌入──▶ q ──向量 + BM25 混合检索──▶ Hit 列表
//! ```
//!
//! # 跑法
//!
//! ```bash
//! # 仓库根放 .env(已 gitignore)或直接 export,二选一填 key:
//! #   EXAMPLE_EMBEDDING_API_KEY=sk-...
//! #   OPENAI_API_KEY=sk-...
//! #
//! # 默认连 OpenAI 官方;连 OpenRouter 等兼容端点加两行:
//! #   MNEME_EMBEDDING_BASE_URL=https://openrouter.ai/api/v1
//! #   MNEME_EMBEDDING_MODEL=openai/text-embedding-3-small
//! cargo run --example memory
//! ```
//!
//! 启动时用一句探测文本向模型量一次维度(维度是建库属性;已有库维度不符会被拒绝,
//! 见 `docs/design/16-api-reference.md` §6);库默认落在 `target/memory/`
//! (`MNEME_EXAMPLE_PATH` 可改,`target/` 已 gitignore)。命令表见 [`HELP`],
//! `/quit` 或 Ctrl-D 退出,退出时 `close()` 保证已确认写入持久。
//!
//! # 离线验证
//!
//! 示例内置 mock 端点的路径用例(不出网):`cargo test --example memory`。

mod embedding;
// 统一环境变量入口:与测试侧 `tests/common/env.rs` 直接复用同一文件(仅 dev 目标;
// Mneme 本体不读环境变量,变量清单见该文件头)。
#[path = "../../tests/common/env.rs"]
mod env;
#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

use std::io::Write as _;

use crate::env as dev_env;
use embedding::{BoxError, EmbeddingConfig, EmbeddingProvider, EnvFile, OpenAiProvider};
use mneme::{
    Clock, Expr, Metric, Mneme, MnemeError, Namespace, Record, Scoring, SystemClock,
    core::metric::cosine, json,
};

/// 记忆写入的命名空间(按生命周期划分,见设计 13 §1)。
const NAMESPACE: &str = "agent-42/profile";

/// 默认库目录;放在 `target/` 下,示例产物不污染源码树。
const DEFAULT_DB_PATH: &str = "target/memory";

/// 启动探测文本:只为从真实响应里量出向量维度(维度是建库属性,设计 16 §6)。
const PROBE_TEXT: &str = "mneme dimension probe";

/// 交互提示符。
const PROMPT: &str = ">>> ";

/// 单次检索返回条数上限。
const TOP_K: usize = 5;

/// 原始余弦相似度阈值:低于该值的命中视为不相关,直接滤掉。
///
/// 综合分只在候选集内归一化,最高那条恒为 `1.000`,无法据此判断"全都查不到"
/// (设计 10 §2.2);要看相关性只能用原始分。阈值与嵌入模型相关,换模型需重新标定。
const MIN_RAW_SIMILARITY: f32 = 0.35;

/// 未显式指定 `importance=` 时的默认重要度。
const DEFAULT_IMPORTANCE: f32 = 0.5;

/// `/touch` 未显式指定 boost 时的默认强化幅度。
const DEFAULT_TOUCH_BOOST: f32 = 0.05;

/// 列表摘要展示的字符数上限。
const TEXT_PREVIEW_CHARS: usize = 48;

/// 帮助文本(`/help` 与启动时打印)。
const HELP: &str = "\
命令(选项写在正文前,如 /add key=pref.theme 用户喜欢深色模式):
  /add [key=<外部键>] [kind=<元数据>] [importance=<0..1>] <文本>
                          嵌入并写入;同 key 重复写即 Upsert
  /search [kind=<元数据 kind>] <查询文本>
                          向量 + BM25 混合检索,按综合分(相似度/重要度/新鲜度)排序;
                          原始余弦低于阈值的命中不显示,结果中带原始余弦列
  /get <key>              点读一条记忆全文
  /list                   列出全部活记录
  /touch <key> [boost]    访问强化(boost 缺省 0.05)
  /delete <key>           删除一条记忆(打墓碑,空间交给 compaction 回收)
  /help                   显示本帮助
  /quit                   关闭库并退出(Ctrl-D 同)";

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let env = EnvFile::repo();
    let config = EmbeddingConfig::from_env(env)?;
    let db_path = env
        .get(dev_env::EXAMPLE_PATH)
        .unwrap_or_else(|| DEFAULT_DB_PATH.to_owned());
    println!("嵌入端点: {} (模型 {})", config.base_url(), config.model());
    println!("库路径: {db_path}  命名空间: {NAMESPACE}");
    let provider = OpenAiProvider::new(&config);

    // 维度是建库属性:先探测一次拿维度,再建/开库;换模型会被拒绝(设计 16 §6)。
    let probe = provider.embed(&[PROBE_TEXT.to_owned()]).await?;
    let dimension = probe.first().ok_or("嵌入 API 未返回探测向量")?.len();
    let db = open_database(&db_path, dimension as u32)?;
    println!("维度: {dimension}(取自模型响应)\n");
    println!("{HELP}\n");

    let ns = db.namespace(NAMESPACE);
    repl(&provider, &ns).await?;

    db.close()?;
    println!("已关闭:写入已持久");
    Ok(())
}

/// 建库或打开已有库;换模型导致维度不符时给出可操作提示(设计 16 §6)。
///
/// # Arguments
/// * `path` - 库目录。
/// * `dimension` - 本次模型响应量出的向量维度。
///
/// # Errors
/// 已有库维度与 `dimension` 不一致、或引擎打开失败时返回错误。
fn open_database(path: &str, dimension: u32) -> Result<Mneme, BoxError> {
    match Mneme::builder()
        .path(path)
        .dimension(dimension)
        .metric(Metric::Cosine)
        .build()
    {
        Ok(db) => Ok(db),
        Err(MnemeError::DimensionMismatch { expected, got }) => Err(format!(
            "库已存在且维度是 {expected},当前模型给出 {got}:换模型 = 新建库\
             (删除 {path} 后重跑,或换回原模型)"
        )
        .into()),
        Err(other) => Err(other.into()),
    }
}

/// 交互主循环:读一行 → 解析 → 执行;`/quit` 或 EOF 结束。
async fn repl(provider: &OpenAiProvider, ns: &Namespace) -> Result<(), BoxError> {
    loop {
        print!("{PROMPT}");
        std::io::stdout().flush()?;
        let Some(line) = read_line().await? else {
            println!();
            break;
        };
        match parse_command(&line) {
            Command::Add(args) => cmd_add(provider, ns, args).await?,
            Command::Search { kind, query } => cmd_search(provider, ns, kind, query).await?,
            Command::Get(key) => cmd_get(ns, &key).await?,
            Command::List => cmd_list(ns).await?,
            Command::Touch { key, boost } => cmd_touch(ns, &key, boost).await?,
            Command::Delete(key) => cmd_delete(ns, &key).await?,
            Command::Help => println!("{HELP}\n"),
            Command::Quit => break,
            Command::Empty => {}
            Command::Usage(message) => println!("用法有误:{message}(输入 /help 查看命令表)"),
            Command::Unknown(name) => println!("未知命令 {name}(输入 /help 查看命令表)"),
        }
    }
    Ok(())
}

/// 读一行标准输入;EOF 返回 `None`。
///
/// 交互读 stdin 属阻塞 IO,经 `spawn_blocking` 隔离,不在 async 上下文直接阻塞。
async fn read_line() -> Result<Option<String>, BoxError> {
    tokio::task::spawn_blocking(|| -> Result<Option<String>, BoxError> {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(line)),
            Err(error) => Err(error.into()),
        }
    })
    .await?
}

/// 把阻塞的引擎调用挪到 blocking 线程池(设计 16 §5)。
///
/// Mneme 是同步阻塞 API;async 宿主里应经 `spawn_blocking` 隔离,或改用
/// feature `async` 的 `AsyncNamespace` 门面。回调收到引擎调用结果并原样返回。
async fn run_engine<T, F>(op: F) -> Result<T, BoxError>
where
    F: FnOnce() -> Result<T, BoxError> + Send + 'static,
    T: Send + 'static,
{
    // `spawn_blocking` 的 JoinError(线程 panic)与非 Send 无关,直接经 `?` 上抛。
    tokio::task::spawn_blocking(op).await?
}

/// 嵌入单条文本并取出向量;空响应视为错误。
async fn embed_one(provider: &OpenAiProvider, text: &str) -> Result<Vec<f32>, BoxError> {
    let vectors = provider.embed(&[text.to_owned()]).await?;
    vectors
        .into_iter()
        .next()
        .ok_or_else(|| "嵌入 API 未返回向量".into())
}

/// `/add`:嵌入文本并写入;同 `key` 即 Upsert。
async fn cmd_add(provider: &OpenAiProvider, ns: &Namespace, args: AddArgs) -> Result<(), BoxError> {
    let vector = embed_one(provider, &args.text).await?;
    let key = args
        .key
        .unwrap_or_else(|| format!("mem-{}", SystemClock.now_unix_ms()));
    let mut metadata = json!({ "source": "repl" });
    if let Some(kind) = args.kind {
        metadata["kind"] = json!(kind);
    }
    let record = Record::new(vector)
        .key(key.clone())
        .text(args.text)
        .metadata(metadata)
        .importance(args.importance);
    let owner = ns.clone();
    let outcome = run_engine(move || Ok(owner.insert(record)?)).await?;
    println!("已写入 [{key}] {outcome:?}");
    Ok(())
}

/// 计算查询向量与库内向量的原始余弦(与引擎 `Metric::Cosine` 同口径;零向量返回 0)。
fn raw_cosine(query: &[f32], stored: &[f32]) -> f32 {
    let norm_sq = |vector: &[f32]| vector.iter().map(|value| value * value).sum::<f32>();
    cosine(query, stored, norm_sq(query), norm_sq(stored))
}

/// `/search`:嵌入查询文本,先按原始余弦阈值过滤,再按记忆感知排序。
async fn cmd_search(
    provider: &OpenAiProvider,
    ns: &Namespace,
    kind: Option<String>,
    query: String,
) -> Result<(), BoxError> {
    let vector = embed_one(provider, &query).await?;
    let filter = kind.map(|kind| Expr::field("kind").eq(kind));
    let owner = ns.clone();
    let scored = run_engine(move || {
        let mut builder = owner
            .search()
            .vector(&vector)
            .text(&query)
            .score(Scoring {
                w_importance: 0.3,
                w_recency: 0.1,
                ..Scoring::default()
            })
            .top_k(TOP_K);
        if let Some(filter) = filter {
            builder = builder.filter(filter);
        }
        // 综合分在候选集内归一化,最高那条恒为 1.000,判定不了相关性;
        // 因此另取原始余弦,由应用层做阈值过滤(设计 10 §2.2)。
        let mut scored = Vec::new();
        for hit in builder.execute()? {
            let stored = owner.get_vector(hit.rowid)?;
            let similarity = stored.map(|stored| raw_cosine(&vector, &stored));
            scored.push((hit, similarity));
        }
        Ok(scored)
    })
    .await?;

    if scored.is_empty() {
        println!("没有命中(库里可能还是空;先用 /add 写一条)。");
        return Ok(());
    }

    let (kept, dropped): (Vec<_>, Vec<_>) = scored
        .into_iter()
        .partition(|(_, similarity)| similarity.is_none_or(|sim| sim >= MIN_RAW_SIMILARITY));
    if kept.is_empty() {
        println!(
            "没有足够相关的记忆:{} 条命中的原始余弦均低于 {MIN_RAW_SIMILARITY:.2},已按不相关滤掉。",
            dropped.len()
        );
        return Ok(());
    }
    println!("命中 {} 条(综合分降序):", kept.len());
    for (rank, (hit, similarity)) in kept.iter().enumerate() {
        let key = hit.key.as_ref().map_or("-", |key| key.as_str());
        let text = hit.text.as_deref().unwrap_or("-");
        let similarity = similarity.map_or_else(|| "?".to_owned(), |sim| format!("{sim:.3}"));
        println!(
            "{:>2}. 综合分 {:.4}  原始余弦 {similarity}  [{key}] {text}",
            rank + 1,
            hit.score
        );
        if rank == 0 {
            let parts = hit.explain();
            println!(
                "    分项: 相似度 {:.3} + 重要度 {:.3} + 新鲜度 {:.3}",
                parts.sim, parts.importance, parts.recency
            );
        }
    }
    if !dropped.is_empty() {
        println!(
            "(另有 {} 条原始余弦低于 {MIN_RAW_SIMILARITY:.2},已滤掉)",
            dropped.len()
        );
    }
    Ok(())
}

/// `/get`:点读一条记忆全文与元数据。
async fn cmd_get(ns: &Namespace, key: &str) -> Result<(), BoxError> {
    let owner = ns.clone();
    let lookup = key.to_owned();
    let stored =
        run_engine(move || Ok(owner.get(&lookup)?.map(|record| record.to_stored()))).await?;
    match stored {
        Some(record) => {
            println!("key: {}", record.key().unwrap_or("-"));
            println!("text: {}", record.text().unwrap_or("-"));
            println!(
                "importance: {}  metadata: {}",
                record.importance(),
                record.metadata()
            );
            println!("vector: {} 维", record.vector().len());
        }
        None => println!("没有 [{key}] 这条活记录。"),
    }
    Ok(())
}

/// `/list`:列出当前命名空间全部活记录。
async fn cmd_list(ns: &Namespace) -> Result<(), BoxError> {
    let owner = ns.clone();
    let rows = run_engine(move || {
        let mut rows = Vec::new();
        for row in owner.iter(None)? {
            rows.push(row?.to_stored());
        }
        Ok(rows)
    })
    .await?;
    if rows.is_empty() {
        println!("库还是空;先用 /add 写一条。");
        return Ok(());
    }
    println!("活记录 {} 条:", rows.len());
    for record in &rows {
        println!(
            "  [{}] importance={:.1} {}",
            record.key().unwrap_or("-"),
            record.importance(),
            preview(record.text())
        );
    }
    Ok(())
}

/// `/touch`:访问强化(缺省 boost 0.05)。
async fn cmd_touch(ns: &Namespace, key: &str, boost: Option<f32>) -> Result<(), BoxError> {
    let owner = ns.clone();
    let lookup = key.to_owned();
    let boost = boost.unwrap_or(DEFAULT_TOUCH_BOOST);
    let found = run_engine(move || Ok(owner.touch(&lookup, Some(boost))?)).await?;
    if found {
        println!("已强化 [{key}](importance +{boost})");
    } else {
        println!("没有 [{key}] 这条活记录。");
    }
    Ok(())
}

/// `/delete`:按 key 删除(打墓碑)。
async fn cmd_delete(ns: &Namespace, key: &str) -> Result<(), BoxError> {
    let owner = ns.clone();
    let lookup = key.to_owned();
    let deleted = run_engine(move || Ok(owner.delete(&lookup)?)).await?;
    if deleted {
        println!("已删除 [{key}](墓碑可见于 iter_with(.., true),空间交给 compaction)");
    } else {
        println!("没有 [{key}] 这条活记录。");
    }
    Ok(())
}

/// 截断文本做列表摘要;超出上限补省略号。
fn preview(text: Option<&str>) -> String {
    let text = text.unwrap_or("-");
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(TEXT_PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// `/add` 的解析结果。
#[derive(Debug, PartialEq)]
struct AddArgs {
    /// 外部键;`None` 时按时间自动生成。
    key: Option<String>,
    /// 元数据 `kind`;参与过滤 DSL。
    kind: Option<String>,
    /// 重要度,限 `[0,1]`。
    importance: f32,
    /// 正文(嵌入输入)。
    text: String,
}

/// 一条交互命令。
#[derive(Debug, PartialEq)]
enum Command {
    /// 嵌入并写入。
    Add(AddArgs),
    /// 混合检索。
    Search {
        /// 过滤元数据 `kind`。
        kind: Option<String>,
        /// 查询文本。
        query: String,
    },
    /// 点读。
    Get(String),
    /// 列全部。
    List,
    /// 访问强化。
    Touch {
        /// 外部键。
        key: String,
        /// 强化幅度;`None` 用默认值。
        boost: Option<f32>,
    },
    /// 删除。
    Delete(String),
    /// 帮助。
    Help,
    /// 退出。
    Quit,
    /// 空行。
    Empty,
    /// 参数用法错误。
    Usage(String),
    /// 未知命令。
    Unknown(String),
}

/// 解析一行输入(首尾空白忽略);选项必须写在正文前。
fn parse_command(line: &str) -> Command {
    let line = line.trim();
    if line.is_empty() {
        return Command::Empty;
    }
    let (name, rest) = match line.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim_start()),
        None => (line, ""),
    };
    match name {
        "/add" => {
            let (options, text) = match parse_options(rest, &["key", "kind", "importance"]) {
                Ok(parsed) => parsed,
                Err(message) => return Command::Usage(message.to_owned()),
            };
            let text = text.trim();
            if text.is_empty() {
                return Command::Usage("/add 需要正文,如 /add 用户喜欢深色模式".to_owned());
            }
            let importance = match option_value(&options, "importance") {
                None => DEFAULT_IMPORTANCE,
                Some(value) => match value.parse::<f32>() {
                    Ok(parsed) if (0.0..=1.0).contains(&parsed) => parsed,
                    _ => return Command::Usage("importance 需要 0..1 的数字".to_owned()),
                },
            };
            Command::Add(AddArgs {
                key: option_value(&options, "key").map(str::to_owned),
                kind: option_value(&options, "kind").map(str::to_owned),
                importance,
                text: text.to_owned(),
            })
        }
        "/search" => {
            let (options, query) = match parse_options(rest, &["kind"]) {
                Ok(parsed) => parsed,
                Err(message) => return Command::Usage(message.to_owned()),
            };
            let query = query.trim();
            if query.is_empty() {
                return Command::Usage("/search 需要查询文本".to_owned());
            }
            Command::Search {
                kind: option_value(&options, "kind").map(str::to_owned),
                query: query.to_owned(),
            }
        }
        "/get" => required_key("/get", rest),
        "/delete" => required_key("/delete", rest),
        "/touch" => {
            let rest = rest.trim();
            match rest.split_once(char::is_whitespace) {
                None if rest.is_empty() => {
                    Command::Usage("/touch 需要 key,如 /touch pref.theme 0.1".to_owned())
                }
                None => Command::Touch {
                    key: rest.to_owned(),
                    boost: None,
                },
                Some((key, boost_text)) => match boost_text.trim().parse::<f32>() {
                    Ok(boost) if boost.is_finite() => Command::Touch {
                        key: key.to_owned(),
                        boost: Some(boost),
                    },
                    _ => Command::Usage("boost 需要有限数字".to_owned()),
                },
            }
        }
        "/list" => Command::List,
        "/help" => Command::Help,
        "/quit" | "/exit" => Command::Quit,
        other => Command::Unknown(other.to_owned()),
    }
}

/// `/get` 与 `/delete` 共用的"必须有 key"解析。
fn required_key(name: &str, rest: &str) -> Command {
    let key = rest.trim();
    if key.is_empty() {
        return Command::Usage(format!("{name} 需要 key"));
    }
    if name == "/get" {
        Command::Get(key.to_owned())
    } else {
        Command::Delete(key.to_owned())
    }
}

/// 前导选项列表与剩余正文(选项名, 选项值)…。
type ParsedOptions<'a> = (Vec<(&'a str, &'a str)>, &'a str);

/// 解析行首的 `name=value` 选项(只认 `known` 白名单);空值报用法错误。
fn parse_options<'a>(rest: &'a str, known: &[&str]) -> Result<ParsedOptions<'a>, &'static str> {
    let (options, body) = strip_options(rest, known);
    if options.iter().any(|(_, value)| value.is_empty()) {
        return Err("选项值不能为空,如 key=pref.theme");
    }
    Ok((options, body))
}

/// 从行首剥掉 `name=value` 形式的前导选项;只认白名单里的名字,遇正文即停。
fn strip_options<'a>(rest: &'a str, known: &[&str]) -> (Vec<(&'a str, &'a str)>, &'a str) {
    let mut options = Vec::new();
    let mut remaining = rest;
    loop {
        let (token, tail) = match remaining.split_once(char::is_whitespace) {
            Some((token, tail)) => (token, tail.trim_start()),
            None => (remaining, ""),
        };
        let Some((name, value)) = token.split_once('=') else {
            break;
        };
        if !known.contains(&name) {
            break;
        }
        options.push((name, value));
        remaining = tail;
        if remaining.is_empty() {
            break;
        }
    }
    (options, remaining)
}

/// 取选项值;同名选项后出现者生效。
fn option_value<'a>(options: &[(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    options
        .iter()
        .rev()
        .find_map(|(key, value)| (*key == name).then_some(*value))
}
