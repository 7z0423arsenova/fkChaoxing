//! TikuAdapter 开源题库 <https://github.com/DokiDoki1103/tikuAdapter>

use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Result, anyhow};
use regex::Regex;
use reqwest::Client;
use serde_json::{Value, json};
use tracing::error;

use crate::config::Section;
use crate::http::{plain_client, snippet};
use crate::models::{Question, QuestionType, json_str};

static OPTION_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]\.?、?\s?").unwrap());

pub struct TikuAdapter {
    client: Client,
    url: String,
}

impl TikuAdapter {
    pub fn new(section: &Section) -> Result<Self> {
        let url = section
            .get("url")
            .ok_or_else(|| anyhow!("TikuAdapter 未配置 url"))?
            .to_owned();
        Ok(Self {
            client: plain_client(Duration::from_secs(30), None, true)?,
            url,
        })
    }

    pub async fn query(&self, q: &Question) -> Result<Option<String>> {
        let type_code = match q.q_type {
            QuestionType::Single => 0,
            QuestionType::Multiple => 1,
            QuestionType::Completion => 2,
            QuestionType::Judgement => 3,
            _ => 4,
        };
        let options: Vec<String> = q
            .options
            .split('\n')
            .map(|o| OPTION_PREFIX_RE.replace(o, "").into_owned())
            .collect();
        let resp = self
            .client
            .post(&self.url)
            .json(&json!({"question": q.title, "options": options, "type": type_code}))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let text = resp.text().await?;
        let json: Value = serde_json::from_str(&text)?;
        let best: Vec<String> = json
            .get("answer")
            .and_then(|a| a.get("bestAnswer"))
            .and_then(Value::as_array)
            .map(|a| a.iter().map(json_str).collect())
            .unwrap_or_default();
        if best.is_empty() {
            error!("查询失败, 返回：{}", snippet(&text, 300));
            return Ok(None);
        }
        let answer = best.join("\n").trim().to_owned();
        Ok((!answer.is_empty()).then_some(answer))
    }
}
