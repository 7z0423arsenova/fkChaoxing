//! 大模型答题题库（`provider=AI` / `SiliconFlow`）。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures::StreamExt;
use reqwest::Client;
use tracing::{debug, error, info, warn};

use super::QueryContext;
use crate::http::{Http, snippet};
use crate::llm::prompt::{self, ChoiceOption};
use crate::llm::{ChatRequest, LlmClient, LlmConfig, Message, Part, Role, parse};
use crate::models::Question;

/// 每道题最多附带的图片数
const MAX_IMAGES: usize = 4;
/// 单张图片大小上限
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
const IMAGE_REFERER: &str = "https://mooc1.chaoxing.com/";

pub struct LlmTiku {
    name: &'static str,
    client: LlmClient,
    /// 下载题目图片使用学习通会话（携带 Cookie 与浏览器 UA）
    images: Client,
}

impl LlmTiku {
    pub fn new(name: &'static str, cfg: LlmConfig, http: &Arc<Http>) -> Result<Self> {
        info!(
            "已启用{name}: 协议={} 模型={} 地址={}{}",
            cfg.api.as_str(),
            cfg.model,
            cfg.endpoint,
            if cfg.vision {
                " (已开启图片识别)"
            } else {
                ""
            }
        );
        Ok(Self {
            name,
            client: LlmClient::new(cfg)?,
            images: http.client.clone(),
        })
    }

    pub fn label(&self) -> String {
        format!("{}({})", self.name, self.client.config().model)
    }

    pub async fn check_connection(&self) -> bool {
        info!("正在检查 {} 连接...", self.label());
        match self.client.check_connection().await {
            Ok(resp) => {
                let reply = if resp.text.trim().is_empty() {
                    "(仅返回思考内容)".to_owned()
                } else {
                    snippet(resp.text.trim(), 50)
                };
                info!("{} 连接检查成功，回复: {reply}", self.label());
                true
            }
            Err(e) => {
                error!("{} 连接检查失败：{e:#}", self.label());
                false
            }
        }
    }

    pub async fn query_all(
        &self,
        questions: &[Question],
        ctx: &QueryContext<'_>,
    ) -> Vec<Option<String>> {
        let concurrency = self.client.config().concurrency;
        if concurrency <= 1 {
            let mut answers = Vec::with_capacity(questions.len());
            for (i, q) in questions.iter().enumerate() {
                if i > 0 && ctx.delay > 0.0 {
                    tokio::time::sleep(Duration::from_secs_f64(ctx.delay)).await;
                }
                answers.push(self.query(q, ctx).await);
            }
            answers
        } else {
            // 按值传入题目，避免闭包返回的 future 借用流元素，导致整体 future
            // 在 tokio::spawn 中不满足 Send 约束。
            futures::stream::iter(questions.to_vec())
                .map(|question| async move { self.query(&question, ctx).await })
                .buffered(concurrency)
                .collect()
                .await
        }
    }

    pub async fn query(&self, q: &Question, ctx: &QueryContext<'_>) -> Option<String> {
        match self.try_query(q, ctx).await {
            Ok(answer) => answer,
            Err(e) => {
                error!("{} 查询失败：{e:#}", self.label());
                None
            }
        }
    }

    async fn try_query(&self, q: &Question, ctx: &QueryContext<'_>) -> Result<Option<String>> {
        let cfg = self.client.config();
        let options: Vec<ChoiceOption> = prompt::parse_options(q);
        let images = if cfg.vision {
            self.fetch_images(&prompt::image_urls(&q.title)).await
        } else {
            Vec::new()
        };
        let title = prompt::replace_images(&q.title, images.len());
        let course = ctx.course.filter(|_| cfg.course_context);

        let mut parts = vec![Part::Text(prompt::user_prompt(q, &title, &options, course))];
        parts.extend(images);
        let request = ChatRequest {
            system: prompt::system_prompt(q.q_type, cfg.system_prompt.as_deref(), ctx.feedback),
            messages: vec![Message {
                role: Role::User,
                parts,
            }],
            max_tokens: None,
        };
        let resp = self.client.chat(&request).await?;
        debug!(
            "大模型回复 (finish_reason={:?}, tokens={:?}/{:?}): {}",
            resp.finish_reason, resp.input_tokens, resp.output_tokens, resp.text
        );

        let items = parse::extract_answer_items(&resp.text)
            .or_else(|| {
                // 内容为空时（思考模式占满输出），尝试从思考内容中找到最终答案
                resp.text
                    .trim()
                    .is_empty()
                    .then(|| parse::extract_answer_items(&resp.reasoning))
                    .flatten()
            })
            .or_else(|| parse::fallback_items(&resp.text, q.q_type));
        let Some(items) = items else {
            if resp.text.trim().is_empty() {
                warn!(
                    "大模型未返回答案内容（finish_reason={:?}），可能是输出长度不足或思考过程占满了输出，可尝试调大 llm_max_tokens 或关闭思考模式",
                    resp.finish_reason
                );
            } else {
                error!("无法解析大模型输出内容: {}", snippet(&resp.text, 200));
            }
            return Ok(None);
        };
        Ok(parse::finalize_answer(q, &options, &items))
    }

    /// 下载题目中的图片并编码为 Base64；失败的图片会被跳过（题干中保留"[图片]"占位）。
    async fn fetch_images(&self, urls: &[String]) -> Vec<Part> {
        let mut parts = Vec::new();
        for url in urls.iter().take(MAX_IMAGES) {
            match self.fetch_image(url).await {
                Ok(part) => parts.push(part),
                Err(e) => {
                    warn!("下载题目图片失败，将不发送该图片: {url} ({e:#})");
                    break;
                }
            }
        }
        if urls.len() > MAX_IMAGES {
            warn!("题目包含 {} 张图片，只发送前 {MAX_IMAGES} 张", urls.len());
        }
        parts
    }

    async fn fetch_image(&self, url: &str) -> Result<Part> {
        if let Some(rest) = url.strip_prefix("data:") {
            let (meta, data) = rest
                .split_once(',')
                .ok_or_else(|| anyhow::anyhow!("无效的 data URL"))?;
            let bytes = BASE64.decode(data.trim())?;
            let media_type =
                sniff_image_type(&bytes).or_else(|| meta.split(';').next().map(str::to_owned));
            let media_type = media_type.ok_or_else(|| anyhow::anyhow!("不支持的图片格式"))?;
            return Ok(Part::Image {
                media_type,
                data: BASE64.encode(bytes),
            });
        }
        let url = absolute_url(url);
        let resp = self
            .images
            .get(&url)
            .header(reqwest::header::REFERER, IMAGE_REFERER)
            .timeout(Duration::from_secs(20))
            .send()
            .await?
            .error_for_status()?;
        let bytes = resp.bytes().await?;
        anyhow::ensure!(bytes.len() <= MAX_IMAGE_BYTES, "图片超过 5MB");
        let media_type =
            sniff_image_type(&bytes).ok_or_else(|| anyhow::anyhow!("不支持的图片格式"))?;
        Ok(Part::Image {
            media_type,
            data: BASE64.encode(&bytes),
        })
    }
}

fn absolute_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("//") {
        format!("https://{rest}")
    } else if url.starts_with('/') {
        format!("https://mooc1.chaoxing.com{url}")
    } else {
        url.to_owned()
    }
}

/// 根据文件头识别图片类型（只接受各大模型普遍支持的格式）。
fn sniff_image_type(bytes: &[u8]) -> Option<String> {
    let kind = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        return None;
    };
    Some(kind.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ApiKind;
    use crate::models::QuestionType;
    use crate::testutil::{MockResponse, MockServer};
    use serde_json::json;

    #[test]
    fn image_helpers() {
        assert_eq!(
            sniff_image_type(&[0x89, b'P', b'N', b'G', 0, 0]),
            Some("image/png".into())
        );
        assert_eq!(sniff_image_type(b"GIF89a"), Some("image/gif".into()));
        assert_eq!(sniff_image_type(b"<svg></svg>"), None);
        assert_eq!(
            absolute_url("//p.ananas.chaoxing.com/a.png"),
            "https://p.ananas.chaoxing.com/a.png"
        );
        assert_eq!(absolute_url("/x.png"), "https://mooc1.chaoxing.com/x.png");
    }

    fn llm(server: &MockServer, vision: bool) -> LlmTiku {
        let mut cfg = LlmConfig::new(ApiKind::OpenAi, &server.url, "k", "test-model");
        cfg.min_interval = 0.0;
        cfg.vision = vision;
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        LlmTiku::new("AI大模型答题", cfg, &http).unwrap()
    }

    #[tokio::test]
    async fn answers_choice_question_end_to_end() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            json!({"choices": [{"message": {"content": "```json\n{\"answer\": [\"B\"]}\n```"}}]}),
        )])
        .await;
        let tiku = llm(&server, false);
        let options = vec!["A. 上海".to_owned(), "B. 北京".to_owned()];
        let mut q = Question::new(
            "中国的首都是<img src=\"https://x/a.png\">",
            QuestionType::Single,
            &options,
        );
        q.id = "1".into();
        let feedback = vec!["- 题目：示例".to_owned()];
        let ctx = QueryContext {
            delay: 0.0,
            feedback: &feedback,
            course: Some("地理"),
        };
        assert_eq!(tiku.query(&q, &ctx).await.as_deref(), Some("北京"));

        let body = server.requests()[0].json();
        let system = body["messages"][0]["content"].as_str().unwrap();
        let user = body["messages"][1]["content"].as_str().unwrap();
        assert!(system.contains("单选题") && system.contains("- 题目：示例"));
        assert_eq!(
            user,
            "课程：《地理》\n题型：单选题\n题目：中国的首都是[图片]\n选项：\nA. 上海\nB. 北京"
        );
    }

    #[tokio::test]
    async fn vision_sends_data_url_images() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            json!({"choices": [{"message": {"content": "{\"answer\": [\"正确\"]}"}}]}),
        )])
        .await;
        let tiku = llm(&server, true);
        let png = BASE64.encode([0x89, b'P', b'N', b'G', 1, 2, 3]);
        let q = Question::new(
            format!("图中说法正确吗<img src=\"data:image/png;base64,{png}\">"),
            QuestionType::Judgement,
            &[],
        );
        let ctx = QueryContext::default();
        assert_eq!(tiku.query(&q, &ctx).await.as_deref(), Some("正确"));
        let body = server.requests()[0].json();
        let content = &body["messages"][1]["content"];
        assert_eq!(
            content[0]["text"],
            "题型：判断题\n题目：图中说法正确吗[图片1]"
        );
        assert_eq!(
            content[1]["image_url"]["url"],
            format!("data:image/png;base64,{png}")
        );
    }
}
