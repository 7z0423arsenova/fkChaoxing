//! UTF-8 INI 配置文件解析：节名与键名区分大小写，使用 `key = value`。

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::warn;

/// INI 文件中的一个节。
#[derive(Debug, Clone, Default)]
pub struct Section {
    entries: HashMap<String, String>,
}

impl Section {
    /// 原始值（不存在时为 `None`，存在但为空时为 `Some("")`）。
    pub fn raw(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    /// 非空的值，空字符串视为未配置。
    pub fn get(&self, key: &str) -> Option<&str> {
        self.raw(key).map(str::trim).filter(|v| !v.is_empty())
    }

    pub fn get_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).unwrap_or(default)
    }

    /// 设置或覆盖一个配置项（用于命令行参数覆盖配置文件）。
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        self.entries.insert(key.to_owned(), value.into());
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).map(str_to_bool)
    }

    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        self.get_bool(key).unwrap_or(default)
    }

    pub fn get_f64(&self, key: &str) -> Option<f64> {
        let raw = self.get(key)?;
        match raw.parse::<f64>() {
            Ok(v) if v.is_finite() => Some(v),
            _ => {
                warn!("配置项 {key} 的值 {raw:?} 不是有效数字，已忽略");
                None
            }
        }
    }

    pub fn get_u64(&self, key: &str) -> Option<u64> {
        let raw = self.get(key)?;
        match raw.parse::<u64>().ok() {
            Some(v) => Some(v),
            None => {
                warn!("配置项 {key} 的值 {raw:?} 不是有效的非负整数，已忽略");
                None
            }
        }
    }

    /// 以逗号分隔的列表（同时接受中文逗号），自动去除空白与空项。
    pub fn get_list(&self, key: &str) -> Vec<String> {
        self.get(key).map(split_list).unwrap_or_default()
    }
}

/// 解析后的 INI 文件。
#[derive(Debug, Clone, Default)]
pub struct Ini {
    sections: HashMap<String, Section>,
}

impl Ini {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取 UTF-8 配置文件 {}", path.display()))?;
        Ok(Self::parse(&text))
    }

    pub fn parse(text: &str) -> Self {
        let mut sections: HashMap<String, Section> = HashMap::new();
        let mut current: Option<String> = None;

        for (lineno, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
                continue;
            }
            if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                let name = name.trim().to_owned();
                sections.entry(name.clone()).or_default();
                current = Some(name);
                continue;
            }
            let Some(section) = current.as_ref() else {
                warn!("配置文件第 {} 行不属于任何节，已忽略", lineno + 1);
                continue;
            };
            let Some(pos) = trimmed.find('=') else {
                warn!("配置文件第 {} 行格式无效，已忽略", lineno + 1);
                continue;
            };
            let key = trimmed[..pos].trim().to_owned();
            let value = trimmed[pos + 1..].trim().to_owned();
            if key.is_empty() {
                continue;
            }
            let entries = &mut sections.get_mut(section).expect("section exists").entries;
            if entries.insert(key.clone(), value).is_some() {
                warn!("配置项 [{section}] {key} 重复定义，以最后一次为准");
            }
        }
        Self { sections }
    }

    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.get(name)
    }
}

pub fn str_to_bool(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "on"
    )
}

pub fn split_list(value: &str) -> Vec<String> {
    value
        .split([',', '，'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 遇到未开放章节时的处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum NotOpenAction {
    /// 重试（默认）
    #[default]
    Retry,
    /// 询问是否跳过
    Ask,
    /// 直接跳过
    Continue,
}

impl NotOpenAction {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "retry" => Some(Self::Retry),
            "ask" => Some(Self::Ask),
            "continue" => Some(Self::Continue),
            _ => None,
        }
    }
}

/// `[common]` 节对应的运行参数。
#[derive(Debug, Clone)]
pub struct CommonConfig {
    pub use_cookies: bool,
    pub username: String,
    pub password: String,
    pub course_list: Vec<String>,
    pub speed: f64,
    pub jobs: usize,
    pub notopen_action: NotOpenAction,
    pub retry_interval: f64,
    pub work_enabled: bool,
    /// 先尝试按“拖到结尾”上报一次，服务器确认后立即完成。
    pub instant_complete: bool,
    /// 视频等任务运行时，提前在后台查询后续章节检测答案。
    pub work_prefetch_enabled: bool,
    pub work_redo_enabled: bool,
    pub work_max_retries: u32,
    pub add_learning_count: bool,
    pub target_count: u32,
}

impl Default for CommonConfig {
    fn default() -> Self {
        Self {
            use_cookies: false,
            username: String::new(),
            password: String::new(),
            course_list: Vec::new(),
            speed: 1.0,
            jobs: 4,
            notopen_action: NotOpenAction::Retry,
            retry_interval: 1.0,
            work_enabled: true,
            instant_complete: true,
            work_prefetch_enabled: false,
            work_redo_enabled: false,
            work_max_retries: 3,
            add_learning_count: false,
            target_count: 100,
        }
    }
}

impl CommonConfig {
    pub fn from_section(section: &Section) -> Self {
        let mut cfg = Self::default();
        if let Some(v) = section.get_bool("use_cookies") {
            cfg.use_cookies = v;
        }
        cfg.username = section.get_or("username", "").to_owned();
        cfg.password = section.get_or("password", "").to_owned();
        cfg.course_list = section.get_list("course_list");
        if let Some(v) = section.get_f64("speed") {
            cfg.speed = v;
        }
        if let Some(v) = section.get_u64("jobs") {
            cfg.jobs = v as usize;
        }
        if let Some(raw) = section.get("notopen_action") {
            match NotOpenAction::parse(raw) {
                Some(action) => cfg.notopen_action = action,
                None => warn!(
                    "notopen_action 的值 {raw:?} 无效（可选 retry/ask/continue），使用默认值 retry"
                ),
            }
        }
        if let Some(v) = section.get_f64("retry_interval") {
            cfg.retry_interval = v;
        }
        if let Some(v) = section.get_bool("work_enabled") {
            cfg.work_enabled = v;
        }
        if let Some(v) = section.get_bool("instant_complete") {
            cfg.instant_complete = v;
        }
        if let Some(v) = section.get_bool("work_prefetch_enabled") {
            cfg.work_prefetch_enabled = v;
        }
        if let Some(v) = section.get_bool("work_redo_enabled") {
            cfg.work_redo_enabled = v;
        }
        if let Some(v) = section.get_u64("work_max_retries") {
            cfg.work_max_retries = v as u32;
        }
        if let Some(v) = section.get_bool("add_learning_count") {
            cfg.add_learning_count = v;
        }
        if let Some(v) = section.get_u64("target_count") {
            cfg.target_count = v as u32;
        }
        cfg
    }

    /// 修正越界参数：倍速限制在 [1, 2]，并发数至少为 1。
    pub fn normalize(&mut self) {
        self.speed = self.speed.clamp(1.0, 2.0);
        if self.jobs == 0 {
            self.jobs = 4;
        }
        if !self.retry_interval.is_finite() || self.retry_interval < 0.0 {
            self.retry_interval = 1.0;
        }
    }
}

/// 程序的完整配置。
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub common: CommonConfig,
    pub tiku: Section,
    pub captcha: Section,
}

impl AppConfig {
    /// 读取配置文件中的两个节，文件中没有的节为空。
    pub fn from_file(path: &Path) -> Result<Self> {
        let ini = Ini::load(path)?;
        Ok(Self {
            common: ini
                .section("common")
                .map(CommonConfig::from_section)
                .unwrap_or_default(),
            tiku: ini.section("tiku").cloned().unwrap_or_default(),
            captcha: ini.section("captcha").cloned().unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_is_literal_and_colon_is_not_a_separator() {
        let ini = Ini::parse("[common]\npassword = p%%word\nusername: old\n");
        let common = ini.section("common").unwrap();
        assert_eq!(common.get("password"), Some("p%%word"));
        assert_eq!(common.get("username"), None);
    }

    #[test]
    fn non_utf8_config_is_rejected() {
        let path = std::env::temp_dir().join(format!("chaoxing-ini-{}.ini", std::process::id()));
        std::fs::write(&path, b"[common]\nusername = \xff\n").unwrap();
        let result = Ini::load(&path);
        std::fs::remove_file(path).unwrap();
        assert!(result.is_err());
    }

    const SAMPLE: &str = "\u{feff}[common]
; 注释
# 另一种注释
use_cookies=false
username = 13800000000
password = p%w:d=1
course_list = 111, 222，333,,
speed = 3
jobs = 0
notopen_action = continue

[tiku]
provider=TikuGo,AI
endpoint = https://api.example.com/v1
delay=
";

    #[test]
    fn parses_ini() {
        let ini = Ini::parse(SAMPLE);
        let common = ini.section("common").unwrap();
        assert_eq!(common.get("username"), Some("13800000000"));
        assert_eq!(common.get("password"), Some("p%w:d=1"));
        assert_eq!(common.get_list("course_list"), vec!["111", "222", "333"]);

        let tiku = ini.section("tiku").unwrap();
        assert_eq!(tiku.get("provider"), Some("TikuGo,AI"));
        assert_eq!(tiku.get("endpoint"), Some("https://api.example.com/v1"));
        assert_eq!(tiku.raw("delay"), Some(""));
        assert_eq!(tiku.get("delay"), None);
    }

    #[test]
    fn common_config_normalization() {
        let ini = Ini::parse(SAMPLE);
        let mut common = CommonConfig::from_section(ini.section("common").unwrap());
        common.normalize();
        assert_eq!(common.speed, 2.0);
        assert_eq!(common.jobs, 4);
        assert_eq!(common.notopen_action, NotOpenAction::Continue);
        assert!(!common.use_cookies);
        assert_eq!(common.work_max_retries, 3);
    }

    #[test]
    fn work_enabled_defaults_to_true_and_accepts_explicit_values() {
        for (entry, expected) in [
            ("", true),
            ("work_enabled = true", true),
            ("work_enabled = false", false),
        ] {
            let ini = Ini::parse(&format!("[common]\n{entry}\n"));
            assert_eq!(
                CommonConfig::from_section(ini.section("common").unwrap()).work_enabled,
                expected
            );
        }
        assert!(CommonConfig::default().work_enabled);
    }

    #[test]
    fn work_prefetch_defaults_to_false_and_accepts_explicit_values() {
        for (entry, expected) in [
            ("", false),
            ("work_prefetch_enabled = true", true),
            ("work_prefetch_enabled = false", false),
        ] {
            let ini = Ini::parse(&format!("[common]\n{entry}\n"));
            assert_eq!(
                CommonConfig::from_section(ini.section("common").unwrap()).work_prefetch_enabled,
                expected
            );
        }
    }

    #[test]
    fn bool_parsing_accepts_common_values() {
        for v in ["1", "true", "YES", "y", "On"] {
            assert!(str_to_bool(v));
        }
        for v in ["0", "false", "no", "", "maybe"] {
            assert!(!str_to_bool(v));
        }
    }
}
