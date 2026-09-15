//! 宿主侧嵌入:OpenAI 协议客户端 + `.env` / 环境变量配置。
//!
//! Mneme 本体只存向量、零网络依赖(设计 16 §6);"文本 → 向量"永远由宿主负责。
//! 本模块把它收敛成一个 [`EmbeddingProvider`] trait 与一个 OpenAI 协议实现:
//! 换 Gemini / Claude / 本地模型时新增一个 `impl` 即可,主流程不动。
//!
//! OpenAI 兼容端点(OpenRouter、vLLM、Ollama 的 `/v1`、LM Studio……)都实现同一
//! `POST {base_url}/embeddings`,改 `MNEME_EMBEDDING_BASE_URL` 即可切换。

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::embeddings::CreateEmbeddingRequestArgs;

use crate::env as dev_env;

pub use crate::env::EnvFile;

/// 示例统一错误类型;生产代码可换成 `anyhow` 或自定义 `thiserror` 枚举。
///
/// 带 `Send + Sync` 是为了能跨 `spawn_blocking` 线程边界传递(见 `main.rs` 的
/// [`run_engine`])。
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// 默认嵌入端点(OpenAI 官方;换兼容端点请设 `MNEME_EMBEDDING_BASE_URL`)。
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// 默认嵌入模型(1536 维;换模型 = 换库,见设计 16 §6)。
const DEFAULT_MODEL: &str = "text-embedding-3-small";

/// 嵌入端点配置(`.env` / 环境变量全量可覆盖)。
pub struct EmbeddingConfig {
    base_url: String,
    model: String,
    api_key: String,
}

impl EmbeddingConfig {
    /// 从环境读取配置。
    ///
    /// 读取顺序(前者优先):
    /// - API key:`EXAMPLE_EMBEDDING_API_KEY` → `OPENAI_API_KEY`(必填);
    /// - 端点:`MNEME_EMBEDDING_BASE_URL` → `OPENAI_BASE_URL` → [`DEFAULT_BASE_URL`];
    /// - 模型:`MNEME_EMBEDDING_MODEL` → [`DEFAULT_MODEL`]。
    ///
    /// # Arguments
    /// * `env` - 已加载的 `.env`(见 [`EnvFile::load`])。
    ///
    /// # Returns
    /// 构造好的配置;`api_key` 只驻留内存,任何输出都不打印。
    ///
    /// # Errors
    /// 两个 key 变量都缺失时返回错误;报错只含变量名,不回显任何值。
    pub fn from_env(env: &EnvFile) -> Result<Self, BoxError> {
        let api_key = env
            .get(dev_env::EXAMPLE_API_KEY)
            .or_else(|| env.get(dev_env::OPENAI_API_KEY))
            .ok_or("缺少嵌入 API key:请设置 EXAMPLE_EMBEDDING_API_KEY 或 OPENAI_API_KEY")?;
        let base_url = env
            .get(dev_env::EMBEDDING_BASE_URL)
            .or_else(|| env.get(dev_env::OPENAI_BASE_URL))
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        let model = env
            .get(dev_env::EMBEDDING_MODEL)
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
        Ok(Self {
            base_url,
            model,
            api_key,
        })
    }

    /// 端点 base URL(不含 `/embeddings` 后缀;可安全打印)。
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 嵌入模型名(可安全打印)。
    pub fn model(&self) -> &str {
        &self.model
    }
}

/// 手工实现 `Debug`:key 打码,避免日志/断言消息意外回显密钥。
impl std::fmt::Debug for EmbeddingConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddingConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
impl EmbeddingConfig {
    /// 测试用构造:直接给定端点、模型与假 key,完全不读环境变量。
    ///
    /// # Arguments
    /// * `base_url` - 指向本地 mock 服务的 base URL。
    /// * `model` - 模型名(mock 不校验)。
    /// * `api_key` - 假 key(mock 不校验)。
    pub fn for_test(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key: api_key.into(),
        }
    }
}

/// 嵌入提供方:批量文本 → 等长向量,返回顺序与入参一致。
///
/// 先用 OpenAI 协议([`OpenAiProvider`]);接别的厂商时在这里加实现即可。
pub trait EmbeddingProvider {
    /// 批量嵌入 `texts`,返回同序向量。
    ///
    /// # Arguments
    /// * `texts` - 待嵌入文本;空切片返回空向量表(不发请求)。
    ///
    /// # Errors
    /// 网络、鉴权、限流或响应解析失败时返回错误。
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, BoxError>;
}

/// OpenAI 协议的嵌入实现:`POST {base_url}/embeddings`。
pub struct OpenAiProvider {
    client: Client<OpenAIConfig>,
    model: String,
}

impl OpenAiProvider {
    /// 按配置构造客户端。
    ///
    /// # Arguments
    /// * `config` - 端点、模型与 API key;见 [`EmbeddingConfig::from_env`]。
    pub fn new(config: &EmbeddingConfig) -> Self {
        let openai = OpenAIConfig::new()
            .with_api_key(config.api_key.clone())
            .with_api_base(config.base_url.clone());
        Self {
            client: Client::with_config(openai),
            model: config.model.clone(),
        }
    }
}

impl EmbeddingProvider for OpenAiProvider {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, BoxError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let request = CreateEmbeddingRequestArgs::default()
            .model(&self.model)
            .input(texts.to_vec())
            .build()
            .map_err(|error| format!("构造嵌入请求失败: {error}"))?;
        let response = self
            .client
            .embeddings()
            .create(request)
            .await
            .map_err(|error| format!("嵌入 API 调用失败: {error}"))?;
        // OpenAI 协议用 `index` 标识输入顺序;兼容端点不保证按序返回,排序后再对齐。
        let mut data = response.data;
        data.sort_by_key(|item| item.index);
        Ok(data.into_iter().map(|item| item.embedding).collect())
    }
}
