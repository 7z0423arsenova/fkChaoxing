//! LIKE 知识库 <https://www.datam.site/>

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Result, bail};
use rand::seq::IndexedRandom;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tracing::{error, info, warn};

use crate::config::Section;
use crate::http::{plain_client, snippet};
use crate::models::{Question, QuestionType, json_str};

const QUERY_API: &str = "https://app.datam.site/api/v1/query";
const BALANCE_API: &str = "https://app.datam.site/api/v1/balance";

pub struct TikuLike {
    client: Client,
    model: String,
    search: bool,
    vision: bool,
    retry: bool,
    retry_times: u32,
    tokens: Vec<String>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    balance: HashMap<String, i64>,
    count: u32,
}

fn mask(token: &str) -> String {
    let tail: String = token
        .chars()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("...{tail}")
}

impl TikuLike {
    pub async fn new(section: &Section) -> Result<Self> {
        let tokens = section.get_list("tokens");
        if tokens.is_empty() {
            bail!("LIKE知识库初始化失败: 未加载任何有效的Token");
        }
        let like = Self {
            client: plain_client(Duration::from_secs(300), None, true)?,
            model: section.get_or("likeapi_model", "").to_owned(),
            search: section.bool_or("likeapi_search", false),
            vision: section.bool_or("likeapi_vision", true),
            retry: section.bool_or("likeapi_retry", true),
            retry_times: section.get_u64("likeapi_retry_times").unwrap_or(3).max(1) as u32,
            tokens,
            state: Mutex::new(State::default()),
        };
        like.update_balances().await;
        Ok(like)
    }

    async fn balance_of(&self, token: &str) -> i64 {
        let resp = self.client.get(BALANCE_API).bearer_auth(token).send().await;
        match resp {
            Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                Ok(v) => v
                    .get("balance")
                    .map(json_str)
                    .and_then(|b| b.parse::<f64>().ok())
                    .map_or(0, |b| b as i64),
                Err(_) => {
                    error!("LIKE知识库余额响应解析失败: 响应格式不正确");
                    0
                }
            },
            Ok(r) => {
                error!("LIKE知识库请求余额接口失败，状态码: {}", r.status());
                0
            }
            Err(e) => {
                error!("LIKE知识库Token余额查询过程中出现错误: {e}");
                0
            }
        }
    }

    async fn update_balances(&self) {
        for token in &self.tokens {
            let balance = self.balance_of(token).await;
            info!(
                "当前LIKE知识库Token: {} 的剩余查询次数为: {balance} (仅供参考, 实际次数以查询结果为准)",
                mask(token)
            );
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .balance
                .insert(token.clone(), balance);
        }
    }

    fn pick_token(&self) -> Option<String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut rng = rand::rng();
        let token = self.tokens.choose(&mut rng)?.clone();
        if state.balance.get(&token).copied().unwrap_or(0) > 0 {
            return Some(token);
        }
        error!("LIKE知识库当前Token查询次数不足: {}", mask(&token));
        let available: Vec<&String> = self
            .tokens
            .iter()
            .filter(|t| state.balance.get(*t).copied().unwrap_or(0) > 0)
            .collect();
        match available.choose(&mut rng) {
            Some(t) => Some((*t).clone()),
            None => {
                error!("LIKE知识库所有Token查询次数都不足");
                None
            }
        }
    }

    pub async fn query(&self, q: &Question) -> Result<Option<String>> {
        let prefix = match q.q_type {
            QuestionType::Single => "【单选题】",
            QuestionType::Multiple => "【多选题】",
            QuestionType::Completion => "【填空题】",
            QuestionType::Judgement => "【判断题】",
            _ => "【其他类型题目】",
        };
        let mut question = format!("{prefix}{}\n", q.title);
        if q.q_type.is_choice() {
            question.push_str(&format!("选项为: {}\n", q.options));
        }
        let Some(token) = self.pick_token() else {
            return Ok(None);
        };

        let attempts = if self.retry { self.retry_times } else { 1 };
        let mut answer = None;
        for attempt in 1..=attempts {
            answer = self.query_single(&token, &question).await;
            if answer.is_some() {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let balance = state.balance.entry(token.clone()).or_insert(0);
                *balance -= 1;
                info!("使用Token {} 查询成功，剩余次数: {balance}", mask(&token));
                break;
            }
            if attempt < attempts {
                warn!(
                    "使用Token {} 查询失败，进行第 {} 次重试...",
                    mask(&token),
                    attempt + 1
                );
            }
        }

        // 每 10 次查询后刷新余额
        let refresh = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.count = (state.count + 1) % 10;
            state.count == 0
        };
        if refresh {
            self.update_balances().await;
        }
        Ok(answer)
    }

    async fn query_single(&self, token: &str, query: &str) -> Option<String> {
        let body = json!({"query": query, "model": self.model, "search": self.search, "vision": self.vision});
        let resp = match self
            .client
            .post(QUERY_API)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                error!("LIKE知识库查询超时: 请求超过300秒");
                return None;
            }
            Err(e) if e.is_connect() => {
                error!("LIKE知识库网络连接错误: 无法连接到API服务器");
                return None;
            }
            Err(e) => {
                error!("LIKE知识库查询异常: {e}");
                return None;
            }
        };
        match resp.status() {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED => {
                error!("LIKE知识库认证失败: 请检查Token是否正确或已过期");
                return None;
            }
            StatusCode::TOO_MANY_REQUESTS => {
                error!("LIKE知识库请求过于频繁: 已达到API速率限制");
                return None;
            }
            StatusCode::FORBIDDEN => {
                error!("LIKE知识库访问被拒绝: 可能是Token权限不足");
                return None;
            }
            status => {
                error!(
                    "LIKE知识库查询失败: 状态码 {status}, 响应内容: {}",
                    snippet(&resp.text().await.unwrap_or_default(), 200)
                );
                return None;
            }
        }
        let Ok(json) = resp.json::<Value>().await else {
            error!("LIKE知识库响应解析失败: 响应不是有效的JSON格式");
            return None;
        };
        if let Some(msg) = json
            .get("message")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
        {
            info!("LIKE知识库响应消息: {msg}");
        }
        let Some(output) = json
            .get("results")
            .and_then(|r| r.get("output"))
            .filter(|o| o.is_object())
        else {
            error!("LIKE知识库查询结果格式错误: results.output 字段不存在");
            return None;
        };
        let q_type = output.get("questionType").and_then(Value::as_str)?;
        let answer = output.get("answer").filter(|a| a.is_object())?;
        extract_answer_by_type(q_type, answer)
    }
}

fn non_empty_items(value: Option<&Value>) -> Option<String> {
    let items: Vec<String> = value?
        .as_array()?
        .iter()
        .filter(|v| !v.is_null())
        .map(json_str)
        .filter(|s| !s.trim().is_empty())
        .collect();
    (!items.is_empty()).then(|| items.join("\n"))
}

fn extract_answer_by_type(q_type: &str, answer: &Value) -> Option<String> {
    let result = match q_type {
        "CHOICE" => non_empty_items(answer.get("selectedOptions")),
        "FILL_IN_BLANK" => non_empty_items(answer.get("blanks")),
        "JUDGMENT" => answer
            .get("isCorrect")
            .and_then(Value::as_bool)
            .map(|b| if b { "正确" } else { "错误" }.to_owned()),
        _ => answer
            .get("otherText")
            .filter(|v| !v.is_null())
            .map(json_str),
    };
    if result.is_none() {
        error!("LIKE知识库 {q_type} 类型题目没有有效的答案内容");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_extracted_by_type() {
        assert_eq!(
            extract_answer_by_type(
                "CHOICE",
                &json!({"selectedOptions": ["北京", null, " ", "上海"]})
            ),
            Some("北京\n上海".into())
        );
        assert_eq!(
            extract_answer_by_type("JUDGMENT", &json!({"isCorrect": false})),
            Some("错误".into())
        );
        assert_eq!(
            extract_answer_by_type("FILL_IN_BLANK", &json!({"blanks": []})),
            None
        );
        assert_eq!(
            extract_answer_by_type("ESSAY", &json!({"otherText": "答"})),
            Some("答".into())
        );
        assert_eq!(mask("abcdefgh"), "...defgh");
    }
}
