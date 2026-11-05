//! GO 题（网课小工具题库）<https://q.icodef.com/>

use std::sync::LazyLock;
use std::time::Duration;

use anyhow::Result;
use regex::Regex;
use reqwest::Client;
use serde_json::Value;
use tracing::{error, info, warn};

use crate::config::Section;
use crate::http::{RateLimiter, plain_client, snippet};
use crate::models::{Question, json_str};

const API: &str = "https://q.icodef.com/wyn-nb?v=4";
const THROTTLE_KEYWORDS: &[&str] = &["流控限制", "速度太快", "并发限制", "忙不过来"];
/// 未搜到答案时接口返回的占位内容
const PLACEHOLDER: &str = "李恒雅";

static BRACKET_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^【[^】]+】\s*").unwrap());
static SQUARE_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[[^\]]+\]\s*").unwrap());

pub struct TikuGo {
    client: Client,
    authorization: String,
    limiter: RateLimiter,
    min_interval: f64,
    retry_times: u32,
    retry_backoff: f64,
}

struct Parsed {
    code: i64,
    answer: String,
    msg: String,
    throttled: bool,
}

impl TikuGo {
    pub fn new(section: &Section) -> Result<Self> {
        let min_interval = match section.get_f64("go_min_interval") {
            Some(v) if v >= 0.0 => v,
            Some(_) => {
                warn!("GO题配置 go_min_interval 无效，使用默认值 1.0");
                1.0
            }
            None => 1.0,
        };
        let retry_times = match section.get_u64("go_retry_times") {
            Some(v) if v >= 1 => v as u32,
            Some(_) => {
                warn!("GO题配置 go_retry_times 无效，使用默认值 3");
                3
            }
            None => 3,
        };
        let retry_backoff = section
            .get_f64("go_retry_backoff")
            .filter(|v| *v >= 0.0)
            .unwrap_or(1.2);
        Ok(Self {
            client: plain_client(Duration::from_secs(15), None, false)?,
            authorization: section.get_or("go_authorization", "").to_owned(),
            limiter: RateLimiter::new(min_interval),
            min_interval,
            retry_times,
            retry_backoff,
        })
    }

    pub async fn query(&self, q: &Question) -> Result<Option<String>> {
        let title = q.title.trim();
        let mut candidates: Vec<String> = Vec::new();
        for c in [
            title.to_owned(),
            BRACKET_PREFIX_RE.replace(title, "").trim().to_owned(),
            SQUARE_PREFIX_RE.replace(title, "").trim().to_owned(),
        ] {
            if !c.is_empty() && !candidates.contains(&c) {
                candidates.push(c);
            }
        }
        for candidate in candidates {
            if let Some(answer) = self.query_once(&candidate).await {
                return Ok(Some(answer));
            }
        }
        Ok(None)
    }

    async fn sleep_retry(&self, attempt: u32, reason: &str, include_min_interval: bool) {
        let mut secs = self.retry_backoff * attempt as f64;
        if include_min_interval {
            secs = secs.max(self.min_interval);
        }
        warn!(
            "GO题{reason}，{secs:.1}s 后重试 ({attempt}/{})",
            self.retry_times
        );
        tokio::time::sleep(Duration::from_secs_f64(secs)).await;
    }

    async fn query_once(&self, question: &str) -> Option<String> {
        for attempt in 1..=self.retry_times {
            self.limiter.wait().await;
            let result = self
                .client
                .post(API)
                .header("Authorization", &self.authorization)
                .form(&[("question", question)])
                .send()
                .await;
            self.limiter.mark_now();
            let resp = match result {
                Ok(resp) => resp,
                Err(e) => {
                    error!("GO题查询异常 ({attempt}/{}): {e}", self.retry_times);
                    if attempt < self.retry_times {
                        self.sleep_retry(attempt, "查询异常", true).await;
                        continue;
                    }
                    break;
                }
            };
            let parsed = parse_response(resp).await?;
            if parsed.code != 1 {
                if parsed.throttled && attempt < self.retry_times {
                    self.sleep_retry(attempt, "触发流控", false).await;
                    continue;
                }
                info!(
                    "GO题未命中或失败: {}",
                    if parsed.msg.is_empty() {
                        "未知错误"
                    } else {
                        &parsed.msg
                    }
                );
                return None;
            }
            if parsed.answer.is_empty() {
                return None;
            }
            if parsed.answer.contains(PLACEHOLDER) || parsed.msg.contains(PLACEHOLDER) {
                if parsed.throttled && attempt < self.retry_times {
                    self.sleep_retry(attempt, "命中流控提示", false).await;
                    continue;
                }
                return None;
            }
            return Some(parsed.answer);
        }
        None
    }
}

async fn parse_response(resp: reqwest::Response) -> Option<Parsed> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        error!(
            "GO题查询失败: 状态码 {status}, 响应: {}",
            snippet(&text, 200)
        );
        return None;
    }
    let Ok(json) = serde_json::from_str::<Value>(&text) else {
        error!(
            "GO题查询失败: 返回内容不是有效JSON, 响应: {}",
            snippet(&text, 200)
        );
        return None;
    };
    let code = json
        .get("code")
        .map(json_str)
        .and_then(|c| c.trim().parse().ok())
        .unwrap_or(0);
    let answer = json
        .get("data")
        .map(json_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let msg = json
        .get("msg")
        .map(json_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let raw = format!("{answer} {msg}");
    let throttled = THROTTLE_KEYWORDS.iter().any(|k| raw.contains(k));
    Some(Parsed {
        code,
        answer,
        msg,
        throttled,
    })
}
