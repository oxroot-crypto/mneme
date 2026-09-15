//! 离线端到端用例:mock 端点 + 内存/临时目录库,覆盖成功与失败路径。
//!
//! 跑法:`cargo test --example memory`(Cargo.toml 里示例标了 `test = true`,
//! CI 的 `cargo test` 矩阵会自动跑到)。全部用例不出网、不依赖真实 API key。

use std::net::TcpListener;

use crate::embedding::{EmbeddingConfig, EmbeddingProvider, EnvFile, OpenAiProvider};
use crate::env as dev_env;
use crate::mock::MockServer;
use crate::{AddArgs, Command, parse_command};
use mneme::{Feedback, Mneme, MnemeError, Record, Scoring, filter, json};

/// 按文本内容映射的确定性 3 维 mock 向量:
/// 含"深色" → `[1,0,0]`,含"咖啡" → `[0,1,0]`,其余 → `[0,0,1]`。
fn deterministic_embeddings(request: &str) -> (u16, String) {
    let parsed: mneme::Meta = request.parse().unwrap_or_else(|_| json!({}));
    let texts: Vec<String> = match parsed.get("input") {
        Some(value) if value.is_string() => {
            vec![value.as_str().unwrap_or_default().to_owned()]
        }
        Some(value) => value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let data: Vec<mneme::Meta> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let embedding = if text.contains("深色") {
                json!([1.0, 0.0, 0.0])
            } else if text.contains("咖啡") {
                json!([0.0, 1.0, 0.0])
            } else {
                json!([0.0, 0.0, 1.0])
            };
            json!({ "object": "embedding", "index": index, "embedding": embedding })
        })
        .collect();
    let response = json!({
        "object": "list",
        "data": data,
        "model": "mock-embedding",
        "usage": { "prompt_tokens": texts.len(), "total_tokens": texts.len() },
    });
    (200, response.to_string())
}

/// 造一个指向 mock 服务、key 为假值的提供方。
fn provider_for(server: &MockServer) -> OpenAiProvider {
    OpenAiProvider::new(&EmbeddingConfig::for_test(
        server.base_url(),
        "mock-model",
        "sk-test",
    ))
}

// ---------- 配置与 `.env` 路径 ----------

#[test]
fn dotenv_file_supports_whitespace_quotes_and_comments() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    std::fs::write(
        &path,
        "# 注释行\nEXAMPLE_EMBEDDING_API_KEY = \"sk-test\"\nMNEME_EMBEDDING_MODEL='m'\n\nMALFORMED\nEMPTY =\n",
    )
    .unwrap();
    let env = EnvFile::load(&path);
    assert_eq!(
        env.get(dev_env::EXAMPLE_API_KEY).as_deref(),
        Some("sk-test")
    );
    assert_eq!(env.get(dev_env::EMBEDDING_MODEL).as_deref(), Some("m"));
    assert_eq!(env.get("MALFORMED"), None, "无 `=` 的行应忽略");
    assert_eq!(env.get("EMPTY"), None, "空白值按未设置处理");
}

#[test]
fn missing_dotenv_file_yields_no_entries() {
    let dir = tempfile::tempdir().unwrap();
    let env = EnvFile::load(dir.path().join("absent.env"));
    assert_eq!(env.get("MNEME_DOTENV_SELFTEST_ABSENT"), None);
}

#[test]
fn config_defaults_and_overrides() {
    let only_key = EnvFile::entries_only(&[(dev_env::EXAMPLE_API_KEY, "sk-a")]);
    let config = EmbeddingConfig::from_env(&only_key).unwrap();
    assert_eq!(config.base_url(), "https://api.openai.com/v1");
    assert_eq!(config.model(), "text-embedding-3-small");

    let full = EnvFile::entries_only(&[
        (dev_env::OPENAI_API_KEY, "sk-b"),
        (dev_env::EMBEDDING_BASE_URL, "http://127.0.0.1:1/v1"),
        (dev_env::EMBEDDING_MODEL, "custom-model"),
    ]);
    let config = EmbeddingConfig::from_env(&full).unwrap();
    assert_eq!(config.base_url(), "http://127.0.0.1:1/v1");
    assert_eq!(config.model(), "custom-model");
}

#[test]
fn config_missing_key_errors_without_echoing_values() {
    let env = EnvFile::entries_only(&[(dev_env::EMBEDDING_MODEL, "m")]);
    let error = EmbeddingConfig::from_env(&env).unwrap_err().to_string();
    assert!(error.contains(dev_env::EXAMPLE_API_KEY), "error={error}");
    assert!(!error.contains("sk-"), "报错不应回显任何疑似密钥: {error}");
}

// ---------- 嵌入客户端 HTTP 路径 ----------

#[tokio::test]
async fn embed_reorders_vectors_by_index() {
    let server = MockServer::spawn(|_| {
        (
            200,
            json!({
                "object": "list",
                "data": [
                    { "object": "embedding", "index": 1, "embedding": [0.0, 1.0] },
                    { "object": "embedding", "index": 0, "embedding": [1.0, 0.0] },
                ],
                "model": "mock-embedding",
                "usage": { "prompt_tokens": 2, "total_tokens": 2 },
            })
            .to_string(),
        )
    });
    let provider = provider_for(&server);
    let vectors = provider
        .embed(&["first".to_owned(), "second".to_owned()])
        .await
        .unwrap();
    assert_eq!(
        vectors,
        vec![vec![1.0, 0.0], vec![0.0, 1.0]],
        "响应乱序时按 index 归位"
    );
}

#[tokio::test]
async fn embed_batches_all_texts_into_one_request() {
    let server = MockServer::spawn(deterministic_embeddings);
    let provider = provider_for(&server);
    let texts: Vec<String> = ["用户喜欢深色模式", "用户喝咖啡不加糖", "用户喜欢听莫扎特"]
        .iter()
        .map(|text| (*text).to_owned())
        .collect();
    let vectors = provider.embed(&texts).await.unwrap();
    assert_eq!(vectors.len(), 3);
    assert_eq!(vectors[0], vec![1.0, 0.0, 0.0]);
    assert_eq!(vectors[1], vec![0.0, 1.0, 0.0]);
    assert_eq!(vectors[2], vec![0.0, 0.0, 1.0]);

    let requests = server.requests();
    assert_eq!(requests.len(), 1, "批量嵌入应只发一次请求");
    let body: mneme::Meta = requests[0].parse().unwrap();
    assert_eq!(body["model"], "mock-model");
    assert_eq!(body["input"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn embed_empty_input_skips_network() {
    // 端点故意指向不会监听的端口:只要返回 Ok 就证明根本没发请求。
    let provider = OpenAiProvider::new(&EmbeddingConfig::for_test(
        "http://127.0.0.1:9",
        "mock-model",
        "sk-test",
    ));
    let vectors = provider.embed(&[]).await.unwrap();
    assert!(vectors.is_empty());
}

#[tokio::test]
async fn embed_surfaces_unauthorized() {
    let server = MockServer::spawn(|_| {
        (
            401,
            json!({ "error": { "message": "invalid api key", "type": "invalid_request_error" } })
                .to_string(),
        )
    });
    let provider = provider_for(&server);
    let error = provider
        .embed(&["x".to_owned()])
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("嵌入 API 调用失败"), "error={error}");
    assert!(error.contains("invalid api key"), "error={error}");
}

#[tokio::test]
async fn embed_surfaces_malformed_response() {
    let server = MockServer::spawn(|_| (200, "this is not json".to_owned()));
    let provider = provider_for(&server);
    let error = provider
        .embed(&["x".to_owned()])
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("嵌入 API 调用失败"), "error={error}");
}

#[tokio::test]
async fn embed_surfaces_connection_error() {
    // 先绑一个回环端口拿到系统分配的空闲端口号,随即释放:连接必被拒绝。
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let provider = OpenAiProvider::new(&EmbeddingConfig::for_test(
        format!("http://127.0.0.1:{port}"),
        "mock-model",
        "sk-test",
    ));
    let error = provider
        .embed(&["x".to_owned()])
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("嵌入 API 调用失败"), "error={error}");
}

// ---------- 嵌入 → 写入 → 检索 端到端路径 ----------

#[tokio::test]
async fn memory_roundtrip_embed_store_search_feedback() {
    let server = MockServer::spawn(deterministic_embeddings);
    let provider = provider_for(&server);

    let memories = [
        ("pref.theme", "用户喜欢深色模式", "preference"),
        ("pref.drink", "用户喝咖啡不加糖", "fact"),
        ("taste.music", "用户喜欢听莫扎特", "preference"),
    ];
    let texts: Vec<String> = memories
        .iter()
        .map(|(_, text, _)| (*text).to_owned())
        .collect();
    let vectors = provider.embed(&texts).await.unwrap();

    let db = Mneme::in_memory(3).unwrap();
    let ns = db.namespace("agent-42/profile");
    let records: Vec<Record> = memories
        .iter()
        .zip(vectors)
        .map(|((key, text, kind), vector)| {
            Record::new(vector)
                .key(*key)
                .text(*text)
                .metadata(json!({ "kind": *kind }))
                .importance(0.8)
        })
        .collect();
    ns.insert_batch(records).unwrap();

    let query = provider.embed(&["深色界面".to_owned()]).await.unwrap();
    let hits = ns
        .search()
        .vector(&query[0])
        .text("深色")
        .filter(filter!(r#"kind == "preference""#))
        .score(Scoring {
            w_importance: 0.2,
            ..Scoring::default()
        })
        .top_k(2)
        .execute()
        .unwrap();
    assert_eq!(hits.len(), 2, "过滤后只应剩 preference 两条");
    assert_eq!(hits[0].key.as_ref().unwrap().as_str(), "pref.theme");
    assert!(hits[0].explain().importance > 0.0, "综合分应含重要度贡献");

    let first = ns
        .feedback(hits[0].rowid, Feedback::Used, hits[0].query_id)
        .unwrap();
    let repeated = ns
        .feedback(hits[0].rowid, Feedback::Used, hits[0].query_id)
        .unwrap();
    assert!(first, "首次反馈生效");
    assert!(!repeated, "同一 (rowid, query_id) 反馈幂等");
    assert_eq!(ns.count(None).unwrap(), 3);

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "文档与查询各一次嵌入请求");
    let query_request: mneme::Meta = requests[1].parse().unwrap();
    assert_eq!(query_request["model"], "mock-model");
}

// ---------- 交互命令解析 ----------

#[test]
fn parse_add_takes_defaults_and_trims_body() {
    assert_eq!(
        parse_command("/add 用户喜欢深色模式  "),
        Command::Add(AddArgs {
            key: None,
            kind: None,
            importance: 0.5,
            text: "用户喜欢深色模式".to_owned(),
        })
    );
}

#[test]
fn parse_add_reads_leading_options() {
    assert_eq!(
        parse_command("/add key=pref.theme kind=preference importance=0.9 用户喜欢深色模式"),
        Command::Add(AddArgs {
            key: Some("pref.theme".to_owned()),
            kind: Some("preference".to_owned()),
            importance: 0.9,
            text: "用户喜欢深色模式".to_owned(),
        })
    );
}

#[test]
fn parse_add_keeps_body_with_equals() {
    // 正文里出现的 `=` 不在白名单,不会当成选项。
    assert_eq!(
        parse_command("/add 配置 a=1 且 b=2"),
        Command::Add(AddArgs {
            key: None,
            kind: None,
            importance: 0.5,
            text: "配置 a=1 且 b=2".to_owned(),
        })
    );
}

#[test]
fn parse_add_rejects_bad_usage_instead_of_silently_defaulting() {
    assert!(matches!(parse_command("/add"), Command::Usage(_)));
    assert!(matches!(parse_command("/add key= x"), Command::Usage(_)));
    assert!(matches!(
        parse_command("/add importance=2 x"),
        Command::Usage(_)
    ));
    assert!(matches!(
        parse_command("/add importance=abc x"),
        Command::Usage(_)
    ));
}

#[test]
fn parse_search_reads_kind_filter() {
    assert_eq!(
        parse_command("/search kind=preference 他喜欢什么界面风格"),
        Command::Search {
            kind: Some("preference".to_owned()),
            query: "他喜欢什么界面风格".to_owned(),
        }
    );
    assert!(matches!(parse_command("/search"), Command::Usage(_)));
}

#[test]
fn parse_misc_commands_and_unknown_input() {
    assert_eq!(
        parse_command("/get pref.theme"),
        Command::Get("pref.theme".to_owned())
    );
    assert_eq!(
        parse_command("/delete pref.theme"),
        Command::Delete("pref.theme".to_owned())
    );
    assert_eq!(
        parse_command("/touch pref.theme 0.2"),
        Command::Touch {
            key: "pref.theme".to_owned(),
            boost: Some(0.2),
        }
    );
    assert_eq!(
        parse_command("/touch pref.theme"),
        Command::Touch {
            key: "pref.theme".to_owned(),
            boost: None,
        }
    );
    assert!(matches!(parse_command("/touch"), Command::Usage(_)));
    assert!(matches!(
        parse_command("/touch pref.theme abc"),
        Command::Usage(_)
    ));
    assert_eq!(parse_command("/list"), Command::List);
    assert_eq!(parse_command("/help"), Command::Help);
    assert_eq!(parse_command("/quit"), Command::Quit);
    assert_eq!(parse_command("   "), Command::Empty);
    assert_eq!(parse_command("hello"), Command::Unknown("hello".to_owned()));
}

// ---------- 原始相似度阈值 ----------

#[test]
fn raw_cosine_handles_known_geometry() {
    assert!(
        (crate::raw_cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6,
        "同向向量应为 1"
    );
    assert!(
        crate::raw_cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6,
        "正交向量应为 0"
    );
    assert_eq!(
        crate::raw_cosine(&[0.0, 0.0], &[1.0, 0.0]),
        0.0,
        "零向量按 0 处理,不产生 NaN"
    );
}

// ---------- 引擎侧边界与持久化路径 ----------

#[test]
fn upsert_same_key_keeps_single_row() {
    let db = Mneme::in_memory(3).unwrap();
    let ns = db.namespace("mem");
    ns.insert(Record::new(vec![1.0, 0.0, 0.0]).key("k").text("v1"))
        .unwrap();
    ns.insert(Record::new(vec![0.0, 1.0, 0.0]).key("k").text("v2"))
        .unwrap();
    assert_eq!(ns.count(None).unwrap(), 1, "同 key Upsert 不新增行");
    let stored = ns.get("k").unwrap().unwrap().to_stored();
    assert_eq!(stored.vector(), &[0.0, 1.0, 0.0], "保留最新版本");
}

#[test]
fn batch_rejects_mixed_dimension_atomically() {
    let db = Mneme::in_memory(3).unwrap();
    let ns = db.namespace("mem");
    let result = ns.insert_batch(vec![
        Record::new(vec![1.0, 0.0, 0.0]).key("ok"),
        Record::new(vec![1.0, 0.0]).key("bad"),
    ]);
    assert!(result.is_err(), "维度不一致应整批拒绝");
    assert_eq!(ns.count(None).unwrap(), 0, "拒绝后不应有部分写入");
}

#[test]
fn forget_tombstones_matching_rows() {
    let db = Mneme::in_memory(2).unwrap();
    let ns = db.namespace("mem");
    ns.insert(
        Record::new(vec![1.0, 0.0])
            .key("scratch")
            .metadata(json!({ "kind": "scratch" })),
    )
    .unwrap();
    ns.insert(
        Record::new(vec![0.0, 1.0])
            .key("keep")
            .metadata(json!({ "kind": "keep" })),
    )
    .unwrap();
    let forgotten = ns.forget(filter!(r#"kind == "scratch""#)).unwrap();
    assert_eq!(forgotten, 1);
    assert_eq!(ns.count(None).unwrap(), 1);
    assert!(ns.get("scratch").unwrap().is_none(), "墓碑后点读不可见");
}

#[test]
fn persisted_db_reopens_and_searches() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Mneme::builder()
            .path(dir.path())
            .dimension(3)
            .build()
            .unwrap();
        let ns = db.namespace("mem");
        ns.insert(Record::new(vec![1.0, 0.0, 0.0]).key("a").text("alpha"))
            .unwrap();
        db.close().unwrap();
    }
    let db = Mneme::open(dir.path()).unwrap();
    let ns = db.namespace("mem");
    assert_eq!(ns.count(None).unwrap(), 1);
    let hits = ns
        .search()
        .vector(&[1.0, 0.0, 0.0])
        .top_k(1)
        .execute()
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].key.as_ref().unwrap().as_str(), "a");
    db.close().unwrap();
}

#[test]
fn dimension_mismatch_on_reopen_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Mneme::builder()
            .path(dir.path())
            .dimension(3)
            .build()
            .unwrap();
        db.close().unwrap();
    }
    let result = Mneme::builder().path(dir.path()).dimension(4).build();
    assert!(
        matches!(result, Err(MnemeError::DimensionMismatch { .. })),
        "换模型(维度不同)必须拒绝打开,绝不静默改写"
    );
}
