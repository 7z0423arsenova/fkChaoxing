//! 题库：多种题库/大模型的统一查询流程（缓存 → 按配置顺序依次查询 → 校验 → 写入缓存）。

pub mod adapter;
pub mod ai;
pub mod cache;
pub mod check;
pub mod go;
pub mod like;
pub mod manual;
pub mod matching;
pub mod yanxi;

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use anyhow::{Result, bail};
use regex::Regex;
use tracing::{debug, error, info, warn};

use crate::config::{Section, split_list, str_to_bool};
use crate::http::Http;
use crate::models::Question;
use cache::{AnswerCache, DEFAULT_CACHE_FILE};
use check::check_answer;

/// 题号候选：最多 3 位数字，可带常见分隔符。是否真的删除还要看后面是否紧跟题干。
static LEADING_QUESTION_NO_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\d{1,3}(?:\s*[.、．:：)）]\s*)?").unwrap());
/// 题干允许的起始字符：中文、英文或左括号。
static QUESTION_START_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{Han}A-Za-z（(【\[]").unwrap());
static TRAILING_SCORE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"（\d+\.\d+分）$").unwrap());

/// 题库通用设置（`[tiku]` 中的 submit / cover_rate / true_list / false_list）。
#[derive(Debug, Clone)]
pub struct TikuSettings {
    /// 达到覆盖率后是否提交（否则只保存）
    pub submit: bool,
    /// 最低题库覆盖率
    pub cover_rate: f64,
    pub true_list: Vec<String>,
    pub false_list: Vec<String>,
}

impl Default for TikuSettings {
    fn default() -> Self {
        Self {
            submit: false,
            cover_rate: 0.8,
            true_list: split_list("正确,对,√,是"),
            false_list: split_list("错误,错,×,否,不对,不正确"),
        }
    }
}

impl TikuSettings {
    pub fn from_section(section: &Section) -> Self {
        let mut settings = Self::default();
        settings.submit = section.get("submit").is_some_and(str_to_bool);
        if let Some(v) = section.get_f64("cover_rate") {
            if (0.0..=1.0).contains(&v) {
                settings.cover_rate = v;
            } else {
                warn!(
                    "配置项 [tiku] cover_rate 的值 {v} 不在 0 到 1 之间，使用默认值 {}",
                    settings.cover_rate
                );
            }
        }
        if section.get("true_list").is_some() {
            settings.true_list = section.get_list("true_list");
        }
        if section.get("false_list").is_some() {
            settings.false_list = section.get_list("false_list");
        }
        settings
    }
}

/// 一次查询的上下文。
#[derive(Debug, Clone, Copy, Default)]
pub struct QueryContext<'a> {
    /// 相邻两题之间的查询间隔（秒）
    pub delay: f64,
    /// 章节检测重做时上一轮的错题反馈（非空时跳过缓存）
    pub feedback: &'a [String],
    /// 课程名，供大模型参考
    pub course: Option<&'a str>,
}

/// 已实现的题库
pub enum Provider {
    Yanxi(yanxi::TikuYanxi),
    Go(go::TikuGo),
    Like(like::TikuLike),
    Adapter(adapter::TikuAdapter),
    Llm(ai::LlmTiku),
    Manual(manual::TikuManual),
}

/// 配置中可用的题库名称，区分大小写。
pub const PROVIDER_NAMES: &[&str] = &[
    "TikuYanxi",
    "TikuGo",
    "TikuLike",
    "TikuAdapter",
    "AI",
    "LLM",
    "SiliconFlow",
    "TikuManual",
];

impl Provider {
    async fn create(name: &str, section: &Section, http: &Arc<Http>) -> Result<Self> {
        Ok(match name {
            "TikuYanxi" => Self::Yanxi(yanxi::TikuYanxi::new(section)?),
            "TikuGo" => Self::Go(go::TikuGo::new(section)?),
            "TikuLike" => Self::Like(like::TikuLike::new(section).await?),
            "TikuAdapter" => Self::Adapter(adapter::TikuAdapter::new(section)?),
            "AI" | "LLM" => Self::Llm(ai::LlmTiku::new(
                "AI大模型答题",
                crate::llm::LlmConfig::from_section(section)?,
                http,
            )?),
            "SiliconFlow" => Self::Llm(ai::LlmTiku::new(
                "硅基流动大模型",
                crate::llm::LlmConfig::siliconflow_from_section(section)?,
                http,
            )?),
            "TikuManual" => Self::Manual(manual::TikuManual::new(section)),
            other => bail!("未知题库: {other}"),
        })
    }

    pub fn name(&self) -> String {
        match self {
            Self::Yanxi(_) => "言溪题库".to_owned(),
            Self::Go(_) => "GO题（网课小工具题库）".to_owned(),
            Self::Like(_) => "LIKE知识库".to_owned(),
            Self::Adapter(_) => "TikuAdapter题库".to_owned(),
            Self::Llm(llm) => llm.label(),
            Self::Manual(_) => "手动输入题库".to_owned(),
        }
    }

    pub fn is_manual(&self) -> bool {
        matches!(self, Self::Manual(_))
    }

    pub fn as_llm(&self) -> Option<&ai::LlmTiku> {
        match self {
            Self::Llm(llm) => Some(llm),
            _ => None,
        }
    }

    async fn query_batch(
        &self,
        questions: &[Question],
        ctx: &QueryContext<'_>,
    ) -> Vec<Option<String>> {
        match self {
            Self::Manual(manual) => manual.query_all(questions).await,
            Self::Llm(llm) => llm.query_all(questions, ctx).await,
            _ => {
                let mut answers = Vec::with_capacity(questions.len());
                for q in questions {
                    if ctx.delay > 0.0 {
                        tokio::time::sleep(Duration::from_secs_f64(ctx.delay)).await;
                    }
                    let result = match self {
                        Self::Yanxi(p) => p.query(q).await,
                        Self::Go(p) => p.query(q).await,
                        Self::Like(p) => p.query(q).await,
                        Self::Adapter(p) => p.query(q).await,
                        Self::Llm(_) | Self::Manual(_) => unreachable!(),
                    };
                    answers.push(result.unwrap_or_else(|e| {
                        error!("{} 查询单个题目发生异常: {e:#}", self.name());
                        None
                    }));
                }
                answers
            }
        }
    }
}

/// 题库实例：按配置顺序依次查询多个题库（一个题库未命中时回退到下一个）。
pub struct Tiku {
    providers: Vec<Provider>,
    pub settings: TikuSettings,
    cache: Option<Arc<AnswerCache>>,
}

impl Tiku {
    /// 未配置题库：章节检测将被跳过。
    pub fn disabled() -> Self {
        Self {
            providers: Vec::new(),
            settings: TikuSettings::default(),
            cache: None,
        }
    }

    pub async fn from_config(section: &Section, http: &Arc<Http>) -> Result<Self> {
        let names = split_list(section.get_or("provider", ""));
        if names.is_empty() {
            info!("未找到题库配置, 已忽略题库功能");
            return Ok(Self::disabled());
        }
        let invalid: Vec<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|n| !PROVIDER_NAMES.contains(n))
            .collect();
        if !invalid.is_empty() {
            error!(
                "题库provider配置无效: {}（可选: {}）",
                invalid.join(", "),
                PROVIDER_NAMES.join(", ")
            );
            return Ok(Self::disabled());
        }

        let mut providers = Vec::new();
        if names.len() == 1 {
            providers.push(Provider::create(&names[0], section, http).await?);
        } else {
            for name in &names {
                match Provider::create(name, section, http).await {
                    Ok(p) => providers.push(p),
                    Err(e) => error!("初始化题库 {name} 失败: {e:#}"),
                }
            }
            if providers.is_empty() {
                error!("多题库回退初始化失败: 没有可用题库");
                return Ok(Self::disabled());
            }
            info!(
                "多题库回退已启用，查询顺序: {}",
                providers
                    .iter()
                    .map(Provider::name)
                    .collect::<Vec<_>>()
                    .join(" -> ")
            );
        }

        Ok(Self {
            providers,
            settings: TikuSettings::from_section(section),
            cache: Some(Arc::new(AnswerCache::open(DEFAULT_CACHE_FILE))),
        })
    }

    pub fn is_disabled(&self) -> bool {
        self.providers.is_empty()
    }

    /// 供界面显示的答案来源，例如“言溪题库”或“AI大模型答题”。
    pub fn label(&self) -> String {
        match self.providers.as_slice() {
            [] => "题库".to_owned(),
            [provider] => provider.name(),
            // The window shows one badge per question; the exact fallback provider is in the log.
            _ => "题库".to_owned(),
        }
    }

    /// 是否包含手动输入题库（手动作答时不受覆盖率限制）
    pub fn is_manual(&self) -> bool {
        self.providers.iter().any(Provider::is_manual)
    }

    pub fn llm_providers(&self) -> impl Iterator<Item = &ai::LlmTiku> {
        self.providers.iter().filter_map(Provider::as_llm)
    }

    /// 关闭本地缓存（`ask` 子命令测试题库时使用）
    pub fn without_cache(mut self) -> Self {
        self.cache = None;
        self
    }

    /// 检查所有大模型题库的连接，任一失败返回 false。
    pub async fn check_llm_connection(&self) -> bool {
        let mut ok = true;
        for llm in self.llm_providers() {
            ok &= llm.check_connection().await;
        }
        ok
    }

    /// 查询一组题目的答案，返回与题目一一对应的结果。会就地清理题目标题中的题号与分值。
    pub async fn query_all(
        &self,
        questions: &mut [Question],
        ctx: &QueryContext<'_>,
    ) -> Vec<Option<String>> {
        let mut results = vec![None; questions.len()];
        if self.is_disabled() {
            return results;
        }
        let manual = self.is_manual();
        for q in questions.iter_mut() {
            let cleaned = clean_title(&q.title);
            if !manual {
                debug!("原始标题：{} -> 处理后标题：{cleaned}", q.title);
            }
            q.title = cleaned;
        }

        let mut pending = Vec::new();
        let use_cache = ctx.feedback.is_empty();
        for (i, q) in questions.iter().enumerate() {
            match self
                .cache
                .as_ref()
                .filter(|_| use_cache)
                .and_then(|c| c.get(&q.title))
            {
                Some(answer) => {
                    info!("从缓存中获取答案：{} -> {answer}", q.title);
                    results[i] = Some(answer);
                }
                None => pending.push(i),
            }
        }

        let chained = self.providers.len() > 1;
        for provider in &self.providers {
            if pending.is_empty() {
                break;
            }
            let subset: Vec<Question> = pending.iter().map(|&i| questions[i].clone()).collect();
            let answers = provider.query_batch(&subset, ctx).await;
            let name = provider.name();
            let mut next = Vec::new();
            for (k, &i) in pending.iter().enumerate() {
                let q = &questions[i];
                let answer = answers
                    .get(k)
                    .cloned()
                    .flatten()
                    .map(|a| a.trim().to_owned())
                    .filter(|a| !a.is_empty());
                match answer {
                    Some(answer)
                        if provider.is_manual()
                            || check_answer(&answer, q.q_type, &self.settings) =>
                    {
                        info!("从{name}获取答案：{} -> {answer}", q.title);
                        if let Some(cache) = &self.cache {
                            cache.add(&q.title, &answer);
                        }
                        results[i] = Some(answer);
                    }
                    Some(answer) => {
                        info!("从{name}获取到的答案类型与题目类型不符，已舍弃: {answer}");
                        next.push(i);
                    }
                    None if chained => {
                        info!("{name} 未命中，回退到下一个题库: {}", q.title);
                        next.push(i);
                    }
                    None => {
                        warn!("{name} 未找到答案：{}", q.title);
                        next.push(i);
                    }
                }
            }
            pending = next;
        }
        results
    }
}

/// 去掉题目开头的题号与结尾的分值，例如 "1中国的首都是（2.0分）" -> "中国的首都是"。
///
/// 只有题号后面紧跟题干时才删除，避免把 "2024年"、"2.5米" 这类正常数字当成题号。
pub fn clean_title(title: &str) -> String {
    let title = strip_question_number(title);
    TRAILING_SCORE_RE.replace(&title, "").into_owned()
}

fn strip_question_number(title: &str) -> String {
    let Some(prefix) = LEADING_QUESTION_NO_RE.find(title) else {
        return title.to_owned();
    };
    let rest = &title[prefix.end()..];
    if QUESTION_START_RE.is_match(rest) {
        rest.to_owned()
    } else {
        title.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Ini;

    #[test]
    fn titles_are_cleaned() {
        assert_eq!(clean_title("12中国的首都是（2.0分）"), "中国的首都是");
        assert_eq!(clean_title("3. 题目（1.0分）"), "题目");
        assert_eq!(clean_title("1、题目"), "题目");
        assert_eq!(clean_title("中国的首都是(2.0分)"), "中国的首都是(2.0分)");
    }

    #[test]
    fn real_numbers_are_not_mistaken_for_question_numbers() {
        assert_eq!(clean_title("2024年发生了什么"), "2024年发生了什么");
        assert_eq!(clean_title("2.5米的绳子有多长"), "2.5米的绳子有多长");
        assert_eq!(clean_title("3.14是圆周率"), "3.14是圆周率");
        assert_eq!(clean_title("100 米赛跑"), "100 米赛跑");
    }

    #[test]
    fn settings_from_section() {
        let ini = Ini::parse("[tiku]\nsubmit=true\ncover_rate=0.9\ntrue_list=是, 对\n");
        let s = TikuSettings::from_section(ini.section("tiku").unwrap());
        assert!(s.submit);
        assert_eq!(s.cover_rate, 0.9);
        assert_eq!(s.true_list, vec!["是", "对"]);
        assert_eq!(s.false_list, TikuSettings::default().false_list);
    }

    #[test]
    fn invalid_cover_rate_falls_back_to_default() {
        let ini = Ini::parse("[tiku]\ncover_rate=1.5\n");
        let s = TikuSettings::from_section(ini.section("tiku").unwrap());
        assert_eq!(s.cover_rate, TikuSettings::default().cover_rate);
    }

    #[tokio::test]
    async fn invalid_or_missing_provider_disables_tiku() {
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        for text in [
            "[tiku]\n",
            "[tiku]\nprovider=\n",
            "[tiku]\nprovider=Unknown\n",
            "[tiku]\nprovider=TikuGo,tikugo\n",
        ] {
            let ini = Ini::parse(text);
            let tiku = Tiku::from_config(ini.section("tiku").unwrap(), &http)
                .await
                .unwrap();
            assert!(tiku.is_disabled(), "{text}");
        }
    }
}
