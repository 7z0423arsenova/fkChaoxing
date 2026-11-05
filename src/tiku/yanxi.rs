//! 言溪题库 <https://tk.enncy.cn/>

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Result, bail};
use reqwest::Client;
use serde_json::Value;
use tracing::{error, info};

use crate::config::Section;
use crate::http::{plain_client, snippet};
use crate::models::{Question, json_str, json_truthy};

const API: &str = "https://tk.enncy.cn/query";

pub struct TikuYanxi {
    client: Client,
    tokens: Vec<String>,
    state: Mutex<State>,
}

struct State {
    /// 当前使用的 token 下标
    index: usize,
    /// 剩余查询次数（初始为 100，查询后按接口返回校正，仅供参考）
    times: i64,
}

impl TikuYanxi {
    pub fn new(section: &Section) -> Result<Self> {
        let tokens = section.get_list("tokens");
        if tokens.is_empty() {
            bail!("言溪题库未配置 tokens");
        }
        Ok(Self {
            client: plain_client(Duration::from_secs(30), None, true)?,
            tokens,
            state: Mutex::new(State {
                index: 0,
                times: 100,
            }),
        })
    }

    fn current_token(&self) -> Result<String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match self.tokens.get(state.index) {
            Some(token) => Ok(token.clone()),
            None => bail!("言溪题库 TOKEN 已用完，请更换后重启程序"),
        }
    }

    pub async fn query(&self, q: &Question) -> Result<Option<String>> {
        loop {
            let token = self.current_token()?;
            let resp = self
                .client
                .get(API)
                .query(&[("question", q.title.as_str()), ("token", token.as_str())])
                .send()
                .await?;
            if !resp.status().is_success() {
                let status = resp.status();
                error!(
                    "言溪题库查询失败: HTTP {status} {}",
                    snippet(&resp.text().await.unwrap_or_default(), 200)
                );
                return Ok(None);
            }
            let json: Value = resp.json().await?;
            let data = json.get("data").cloned().unwrap_or(Value::Null);
            let answer = data.get("answer").map(json_str).unwrap_or_default();
            if !json_truthy(json.get("code")) {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.times == 0 || answer.contains("次数不足") {
                    info!("TOKEN查询次数不足, 将会更换并重新搜题");
                    state.index += 1;
                    continue;
                }
                if answer.contains("题库没有该题") || answer.contains("搜不到答案") {
                    info!("言溪题库没有该题的答案: {}", q.title);
                    return Ok(None);
                }
                let times = data
                    .get("times")
                    .map(json_str)
                    .unwrap_or_else(|| format!("{}(仅参考)", state.times));
                error!(
                    "言溪题库查询失败:\n\t剩余查询数{times}:\n\t消息:{}",
                    json.get("message").map(json_str).unwrap_or_default()
                );
                return Ok(None);
            }
            if let Some(times) = data.get("times").and_then(Value::as_i64) {
                self.state.lock().unwrap_or_else(|e| e.into_inner()).times = times;
            }
            let answer = answer.trim();
            return Ok((!answer.is_empty()).then(|| answer.to_owned()));
        }
    }
}
