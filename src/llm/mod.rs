//! 大模型接入。
//!
//! 支持两种接口协议：
//! - `openai`：OpenAI Chat Completions 兼容接口（OpenAI、DeepSeek、硅基流动、Kimi、智谱 GLM、
//!   通义千问、豆包、Gemini 兼容端点、OpenRouter、Ollama、vLLM 等绝大多数服务）；
//! - `anthropic`：Claude Messages 原生接口。
//!
//! 提供服务商预设、请求限速与并发控制、超时/429/5xx 自动重试、图片（多模态）输入，
//! 以及对推理模型输出（`<think>`、`reasoning_content`）的兼容处理。

pub mod parse;
pub mod prompt;

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RETRY_AFTER};
use reqwest::{Client, StatusCode};
use serde_json::{Map, Value, json};
use tokio::sync::Semaphore;
use tracing::{debug, warn};

use crate::config::Section;
use crate::http::{RateLimiter, plain_client, snippet};

/// Anthropic Messages API 版本
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// anthropic 协议必须指定 max_tokens，未配置时使用该值
const ANTHROPIC_DEFAULT_MAX_TOKENS: u32 = 2048;
const RETRY_BACKOFF_CAP: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKind {
    OpenAi,
    Anthropic,
}

impl ApiKind {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "openai" | "openai-compatible" | "chat" | "chat_completions" => Some(Self::OpenAi),
            "anthropic" | "claude" | "messages" => Some(Self::Anthropic),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }
}

/// 常用服务商预设：选择预设后无需手动填写接口地址。
#[derive(Debug, Clone, Copy)]
pub struct Preset {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub label: &'static str,
    pub api: ApiKind,
    pub endpoint: &'static str,
    /// 是否必须提供 API Key（本地部署的服务通常不需要）
    pub requires_key: bool,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "openai",
        aliases: &["gpt"],
        label: "OpenAI",
        api: ApiKind::OpenAi,
        endpoint: "https://api.openai.com/v1",
        requires_key: true,
    },
    Preset {
        name: "anthropic",
        aliases: &["claude"],
        label: "Anthropic Claude",
        api: ApiKind::Anthropic,
        endpoint: "https://api.anthropic.com",
        requires_key: true,
    },
    Preset {
        name: "deepseek",
        aliases: &[],
        label: "DeepSeek",
        api: ApiKind::OpenAi,
        endpoint: "https://api.deepseek.com/v1",
        requires_key: true,
    },
    Preset {
        name: "siliconflow",
        aliases: &["硅基流动"],
        label: "硅基流动",
        api: ApiKind::OpenAi,
        endpoint: "https://api.siliconflow.cn/v1",
        requires_key: true,
    },
    Preset {
        name: "moonshot",
        aliases: &["kimi"],
        label: "Kimi (月之暗面)",
        api: ApiKind::OpenAi,
        endpoint: "https://api.moonshot.cn/v1",
        requires_key: true,
    },
    Preset {
        name: "zhipu",
        aliases: &["glm", "bigmodel"],
        label: "智谱 GLM",
        api: ApiKind::OpenAi,
        endpoint: "https://open.bigmodel.cn/api/paas/v4",
        requires_key: true,
    },
    Preset {
        name: "qwen",
        aliases: &["dashscope", "bailian", "aliyun"],
        label: "通义千问 (阿里云百炼)",
        api: ApiKind::OpenAi,
        endpoint: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        requires_key: true,
    },
    Preset {
        name: "doubao",
        aliases: &["ark", "volcengine"],
        label: "豆包 (火山方舟)",
        api: ApiKind::OpenAi,
        endpoint: "https://ark.cn-beijing.volces.com/api/v3",
        requires_key: true,
    },
    Preset {
        name: "gemini",
        aliases: &["google"],
        label: "Google Gemini",
        api: ApiKind::OpenAi,
        endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
        requires_key: true,
    },
    Preset {
        name: "openrouter",
        aliases: &[],
        label: "OpenRouter",
        api: ApiKind::OpenAi,
        endpoint: "https://openrouter.ai/api/v1",
        requires_key: true,
    },
    Preset {
        name: "ollama",
        aliases: &[],
        label: "Ollama (本地)",
        api: ApiKind::OpenAi,
        endpoint: "http://127.0.0.1:11434/v1",
        requires_key: false,
    },
    Preset {
        name: "lmstudio",
        aliases: &["lm-studio"],
        label: "LM Studio (本地)",
        api: ApiKind::OpenAi,
        endpoint: "http://127.0.0.1:1234/v1",
        requires_key: false,
    },
];

pub fn find_preset(name: &str) -> Option<&'static Preset> {
    let name = name.trim().to_lowercase();
    PRESETS
        .iter()
        .find(|p| p.name == name || p.aliases.iter().any(|a| a.to_lowercase() == name))
}

/// 大模型客户端配置。
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api: ApiKind,
    /// 完整请求地址（如 `.../v1/chat/completions` 或 `.../v1/messages`）
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<u32>,
    pub timeout: Duration,
    pub retries: u32,
    pub min_interval: f64,
    pub concurrency: usize,
    pub proxy: Option<String>,
    pub extra_body: Map<String, Value>,
    pub extra_headers: Vec<(String, String)>,
    pub json_mode: bool,
    /// 是否把题目中的图片发送给模型
    pub vision: bool,
    /// 是否在提示词中附上课程名
    pub course_context: bool,
    /// 追加的自定义系统提示词
    pub system_prompt: Option<String>,
}

impl LlmConfig {
    /// 读取 `provider=AI` 的配置（`[tiku]` 中的 endpoint/key/model 及 `llm_*` 选项）。
    pub fn from_section(section: &Section) -> Result<Self> {
        let preset = match section.get("llm_preset") {
            Some(name) => Some(find_preset(name).ok_or_else(|| {
                let names: Vec<_> = PRESETS.iter().map(|p| p.name).collect();
                anyhow!("llm_preset={name} 无效，可选: {}", names.join(" / "))
            })?),
            None => None,
        };
        if let Some(preset) = preset {
            tracing::debug!("使用大模型服务商预设: {}", preset.label);
        }
        let api = match section.get("llm_api") {
            Some(raw) => ApiKind::parse(raw)
                .ok_or_else(|| anyhow!("llm_api={raw} 无效，可选 openai / anthropic"))?,
            None => preset.map_or(ApiKind::OpenAi, |p| p.api),
        };
        let base = section
            .get("endpoint")
            .or(preset.map(|p| p.endpoint))
            .ok_or_else(|| {
                anyhow!("未配置大模型接口地址 endpoint（也可以通过 llm_preset 选择服务商预设）")
            })?;
        let model = section
            .get("model")
            .ok_or_else(|| anyhow!("未配置大模型名称 model"))?
            .to_owned();
        let api_key = section.get("key").unwrap_or_default().to_owned();
        if api_key.is_empty() && preset.is_none_or(|p| p.requires_key) && !is_local_endpoint(base) {
            warn!("未配置大模型 API Key（key），请求可能会被服务商拒绝");
        }
        let mut cfg = Self::base(api, base, api_key, model, section)?;
        cfg.proxy = section.get("http_proxy").map(str::to_owned);
        Ok(cfg)
    }

    /// 读取 `provider=SiliconFlow` 的配置。
    pub fn siliconflow_from_section(section: &Section) -> Result<Self> {
        let api_key = section
            .get("siliconflow_key")
            .ok_or_else(|| anyhow!("未配置硅基流动 siliconflow_key"))?
            .to_owned();
        let base = section.get_or(
            "siliconflow_endpoint",
            "https://api.siliconflow.cn/v1/chat/completions",
        );
        let model = section
            .get_or("siliconflow_model", "deepseek-ai/DeepSeek-V3")
            .to_owned();
        let mut cfg = Self::base(ApiKind::OpenAi, base, api_key, model, section)?;
        // 硅基流动的默认采样参数。
        cfg.temperature = cfg.temperature.or(Some(0.7));
        cfg.top_p = cfg.top_p.or(Some(0.7));
        cfg.max_tokens = cfg.max_tokens.or(Some(4096));
        Ok(cfg)
    }

    fn base(
        api: ApiKind,
        base: &str,
        api_key: String,
        model: String,
        section: &Section,
    ) -> Result<Self> {
        let extra_body = match section.get("llm_extra_body") {
            Some(raw) => match serde_json::from_str::<Value>(raw) {
                Ok(Value::Object(map)) => map,
                _ => bail!(
                    "llm_extra_body 必须是 JSON 对象，例如 {{\"thinking\": {{\"type\": \"disabled\"}}}}"
                ),
            },
            None => Map::new(),
        };
        let extra_headers = match section.get("llm_extra_headers") {
            Some(raw) => match serde_json::from_str::<Value>(raw) {
                Ok(Value::Object(map)) => map
                    .into_iter()
                    .map(|(k, v)| {
                        (
                            k,
                            v.as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| v.to_string()),
                        )
                    })
                    .collect(),
                _ => {
                    bail!("llm_extra_headers 必须是 JSON 对象，例如 {{\"X-Title\": \"chaoxing\"}}")
                }
            },
            None => Vec::new(),
        };
        Ok(Self {
            api,
            endpoint: resolve_endpoint(api, base),
            api_key,
            model,
            temperature: section.get_f64("llm_temperature"),
            top_p: section.get_f64("llm_top_p"),
            max_tokens: section
                .get_u64("llm_max_tokens")
                .map(|v| v.min(u32::MAX as u64) as u32),
            timeout: Duration::from_secs_f64(
                section.get_f64("llm_timeout").unwrap_or(120.0).max(1.0),
            ),
            retries: section.get_u64("llm_retries").unwrap_or(2).min(10) as u32,
            min_interval: section
                .get_f64("min_interval_seconds")
                .unwrap_or(3.0)
                .max(0.0),
            concurrency: section.get_u64("llm_concurrency").unwrap_or(1).clamp(1, 32) as usize,
            proxy: None,
            extra_body,
            extra_headers,
            json_mode: section.bool_or("llm_json_mode", false),
            vision: section.bool_or("llm_vision", false),
            course_context: section.bool_or("llm_course_context", true),
            system_prompt: section.get("llm_system_prompt").map(str::to_owned),
        })
    }

    /// 测试/编程使用的最小配置
    #[cfg(test)]
    pub fn new(api: ApiKind, base: &str, api_key: &str, model: &str) -> Self {
        Self {
            api,
            endpoint: resolve_endpoint(api, base),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            temperature: None,
            top_p: None,
            max_tokens: None,
            timeout: Duration::from_secs(120),
            retries: 2,
            min_interval: 0.0,
            concurrency: 1,
            proxy: None,
            extra_body: Map::new(),
            extra_headers: Vec::new(),
            json_mode: false,
            vision: false,
            course_context: true,
            system_prompt: None,
        }
    }

    /// DeepSeek V4 默认开启思考模式，可能导致 content 为空。
    fn is_deepseek_v4(&self) -> bool {
        self.endpoint.to_lowercase().contains("api.deepseek.com")
            && self.model.to_lowercase().starts_with("deepseek-v4")
    }
}

/// 根据协议补全请求路径；已经是完整路径时保持不变。
pub fn resolve_endpoint(api: ApiKind, base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    match api {
        ApiKind::OpenAi if base.ends_with("/chat/completions") => base.to_owned(),
        ApiKind::OpenAi => format!("{base}/chat/completions"),
        ApiKind::Anthropic if base.ends_with("/messages") => base.to_owned(),
        ApiKind::Anthropic if base.ends_with("/v1") => format!("{base}/messages"),
        ApiKind::Anthropic => format!("{base}/v1/messages"),
    }
}

fn is_local_endpoint(url: &str) -> bool {
    let lower = url.to_lowercase();
    [
        "://localhost",
        "://127.0.0.1",
        "://0.0.0.0",
        "://[::1]",
        "://192.168.",
        "://10.",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

#[derive(Debug, Clone)]
pub enum Part {
    Text(String),
    /// Base64 编码的图片
    Image {
        media_type: String,
        data: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            parts: vec![Part::Text(text.into())],
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    /// 覆盖配置中的 max_tokens
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub text: String,
    /// 推理模型的思考内容（reasoning_content / thinking）
    pub reasoning: String,
    pub finish_reason: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// 大模型客户端：负责构造请求、限速、重试与解析响应。
pub struct LlmClient {
    cfg: LlmConfig,
    http: Client,
    headers: HeaderMap,
    limiter: RateLimiter,
    semaphore: Semaphore,
}

impl LlmClient {
    pub fn new(cfg: LlmConfig) -> Result<Self> {
        let http = if cfg.proxy.is_none() && is_local_endpoint(&cfg.endpoint) {
            crate::http::local_client(cfg.timeout)?
        } else {
            plain_client(cfg.timeout, cfg.proxy.as_deref(), false)?
        };
        let headers = build_headers(&cfg)?;
        Ok(Self {
            limiter: RateLimiter::new(cfg.min_interval),
            semaphore: Semaphore::new(cfg.concurrency),
            http,
            headers,
            cfg,
        })
    }

    pub fn config(&self) -> &LlmConfig {
        &self.cfg
    }

    /// 发送对话请求，对超时、连接失败、429、5xx 自动重试。
    pub async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
        let body = self.build_body(request);
        let mut attempt = 0;
        loop {
            match self.send_once(&body).await {
                Ok(resp) => return Ok(resp),
                Err(err) if err.retryable && attempt < self.cfg.retries => {
                    attempt += 1;
                    let wait = err
                        .retry_after
                        .unwrap_or_else(|| Duration::from_secs(1 << attempt.min(5)))
                        .min(RETRY_BACKOFF_CAP);
                    warn!(
                        "大模型请求失败（{}），{:.0} 秒后重试 ({attempt}/{})",
                        err.message,
                        wait.as_secs_f64(),
                        self.cfg.retries
                    );
                    tokio::time::sleep(wait).await;
                }
                Err(err) => bail!("{}", err.message),
            }
        }
    }

    async fn send_once(&self, body: &Value) -> std::result::Result<ChatResponse, RequestError> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| RequestError::fatal("客户端已关闭"))?;
        self.limiter.wait().await;
        debug!("大模型请求 -> {} ({})", self.cfg.endpoint, self.cfg.model);
        let resp = self
            .http
            .post(&self.cfg.endpoint)
            .headers(self.headers.clone())
            .json(body)
            .send()
            .await
            .map_err(|e| RequestError {
                retryable: e.is_timeout() || e.is_connect() || e.is_request(),
                retry_after: None,
                message: format!("网络错误: {e}"),
            })?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map(Duration::from_secs_f64);
        let text = resp.text().await.map_err(|e| RequestError {
            retryable: true,
            retry_after: None,
            message: format!("读取响应失败: {e}"),
        })?;
        if !status.is_success() {
            return Err(RequestError {
                retryable: is_retryable_status(status),
                retry_after,
                message: format!("HTTP {} - {}", status.as_u16(), error_message(&text)),
            });
        }
        let value: Value = serde_json::from_str(&text).map_err(|_| {
            RequestError::fatal(format!("响应不是有效的 JSON: {}", snippet(&text, 300)))
        })?;
        self.parse_response(&value)
            .map_err(|e| RequestError::fatal(format!("{e:#}")))
    }

    /// 构造请求体（不同协议格式不同），并合并 `llm_extra_body`。
    pub fn build_body(&self, request: &ChatRequest) -> Value {
        let cfg = &self.cfg;
        let max_tokens = request.max_tokens.or(cfg.max_tokens);
        let mut body = match cfg.api {
            ApiKind::OpenAi => {
                let mut messages = Vec::new();
                if !request.system.is_empty() {
                    messages.push(json!({"role": "system", "content": request.system}));
                }
                messages.extend(request.messages.iter().map(openai_message));
                let mut body = json!({"model": cfg.model, "messages": messages, "stream": false});
                if let Some(v) = max_tokens {
                    body["max_tokens"] = json!(v);
                }
                if cfg.json_mode {
                    body["response_format"] = json!({"type": "json_object"});
                }
                if cfg.is_deepseek_v4() && !cfg.extra_body.contains_key("thinking") {
                    body["thinking"] = json!({"type": "disabled"});
                }
                body
            }
            ApiKind::Anthropic => {
                let messages: Vec<Value> = request.messages.iter().map(anthropic_message).collect();
                let mut body = json!({
                    "model": cfg.model,
                    "max_tokens": max_tokens.unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS),
                    "messages": messages,
                });
                if !request.system.is_empty() {
                    body["system"] = json!(request.system);
                }
                body
            }
        };
        if let Some(t) = cfg.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(p) = cfg.top_p {
            body["top_p"] = json!(p);
        }
        if let Value::Object(map) = &mut body {
            for (k, v) in &cfg.extra_body {
                map.insert(k.clone(), v.clone());
            }
        }
        body
    }

    fn parse_response(&self, value: &Value) -> Result<ChatResponse> {
        if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
            bail!("服务商返回错误: {}", error_message(&err.to_string()));
        }
        match self.cfg.api {
            ApiKind::OpenAi => parse_openai(value),
            ApiKind::Anthropic => parse_anthropic(value),
        }
    }

    /// 发送一个简单问题以验证配置（地址、Key、模型）是否可用。
    pub async fn check_connection(&self) -> Result<ChatResponse> {
        let request = ChatRequest {
            system: String::new(),
            messages: vec![Message::user_text("你好，请回答：1+1 等于几？只回答数字。")],
            max_tokens: match self.cfg.api {
                ApiKind::Anthropic => Some(256),
                ApiKind::OpenAi => None,
            },
        };
        let resp = self.chat(&request).await?;
        if resp.text.trim().is_empty() && resp.reasoning.trim().is_empty() {
            bail!("未收到有效回复");
        }
        Ok(resp)
    }
}

fn build_headers(cfg: &LlmConfig) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    match cfg.api {
        ApiKind::OpenAi => {
            if !cfg.api_key.is_empty() {
                headers.insert(
                    reqwest::header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", cfg.api_key))
                        .context("API Key 中包含非法字符")?,
                );
            }
        }
        ApiKind::Anthropic => {
            if !cfg.api_key.is_empty() {
                headers.insert(
                    "x-api-key",
                    HeaderValue::from_str(&cfg.api_key).context("API Key 中包含非法字符")?,
                );
            }
            headers.insert(
                "anthropic-version",
                HeaderValue::from_static(ANTHROPIC_VERSION),
            );
        }
    }
    for (k, v) in &cfg.extra_headers {
        let name =
            HeaderName::from_bytes(k.as_bytes()).with_context(|| format!("请求头名称无效: {k}"))?;
        let value = HeaderValue::from_str(v).with_context(|| format!("请求头 {k} 的值无效"))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn openai_message(message: &Message) -> Value {
    let content = match message.parts.as_slice() {
        [Part::Text(text)] => json!(text),
        parts => Value::Array(
            parts
                .iter()
                .map(|part| match part {
                    Part::Text(text) => json!({"type": "text", "text": text}),
                    Part::Image { media_type, data } => json!({
                        "type": "image_url",
                        "image_url": {"url": format!("data:{media_type};base64,{data}")}
                    }),
                })
                .collect(),
        ),
    };
    json!({"role": message.role.as_str(), "content": content})
}

fn anthropic_message(message: &Message) -> Value {
    let content: Vec<Value> = message
        .parts
        .iter()
        .map(|part| match part {
            Part::Text(text) => json!({"type": "text", "text": text}),
            Part::Image { media_type, data } => json!({
                "type": "image",
                "source": {"type": "base64", "media_type": media_type, "data": data}
            }),
        })
        .collect();
    json!({"role": message.role.as_str(), "content": content})
}

fn parse_openai(value: &Value) -> Result<ChatResponse> {
    let choice = value
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| anyhow!("响应中没有 choices: {}", snippet(&value.to_string(), 300)))?;
    let message = choice.get("message").cloned().unwrap_or(Value::Null);
    let text = match message.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    };
    let reasoning = ["reasoning_content", "reasoning"]
        .iter()
        .find_map(|k| message.get(*k).and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned();
    let usage = value.get("usage");
    Ok(ChatResponse {
        text,
        reasoning,
        finish_reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_owned),
        input_tokens: usage
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(Value::as_u64),
        output_tokens: usage
            .and_then(|u| u.get("completion_tokens"))
            .and_then(Value::as_u64),
    })
}

fn parse_anthropic(value: &Value) -> Result<ChatResponse> {
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("响应中没有 content: {}", snippet(&value.to_string(), 300)))?;
    let mut text = String::new();
    let mut reasoning = String::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            Some("thinking") => reasoning.push_str(
                block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            _ => {}
        }
    }
    let usage = value.get("usage");
    Ok(ChatResponse {
        text,
        reasoning,
        finish_reason: value
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_owned),
        input_tokens: usage
            .and_then(|u| u.get("input_tokens"))
            .and_then(Value::as_u64),
        output_tokens: usage
            .and_then(|u| u.get("output_tokens"))
            .and_then(Value::as_u64),
    })
}

struct RequestError {
    retryable: bool,
    retry_after: Option<Duration>,
    message: String,
}

impl RequestError {
    fn fatal(message: impl Into<String>) -> Self {
        Self {
            retryable: false,
            retry_after: None,
            message: message.into(),
        }
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
        || status.as_u16() == 529
}

/// 从错误响应中提取可读的错误信息（兼容 OpenAI / Anthropic / 常见国内服务商格式）。
fn error_message(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return snippet(body.trim(), 300);
    };
    let error = value.get("error").unwrap_or(&value);
    let message = error
        .get("message")
        .or_else(|| error.get("msg"))
        .or_else(|| value.get("message"))
        .or_else(|| value.get("msg"))
        .and_then(Value::as_str)
        .or_else(|| error.as_str());
    match message {
        Some(m) => snippet(m, 300),
        None => snippet(body.trim(), 300),
    }
}

#[cfg(test)]
mod tests;
