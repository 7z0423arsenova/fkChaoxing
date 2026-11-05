//! 命令行参数。

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::config::{AppConfig, CommonConfig, NotOpenAction, Section, split_list};
use crate::models::QuestionType;

#[derive(Debug, Parser)]
#[command(
    name = "fkChaoxing",
    version,
    about = "超星学习通 Rust 核心：课程任务、题库查询与桌面桥接",
    after_help = "示例:\n  fkChaoxing courses -c config.ini\n  fkChaoxing study -c config.ini --course-ids 课程ID\n  fkChaoxing ask \"中国的首都是哪里？\" -c config.ini -o \"A. 上海\" -o \"B. 北京\"\n  fkChaoxing replay capture.har --media audio --dry-run"
)]
pub struct Cli {
    /// 使用配置文件运行程序（命令行中显式给出的参数会覆盖配置文件）
    #[arg(short, long, global = true, value_name = "FILE")]
    #[arg(default_value = "config.ini")]
    pub config: PathBuf,

    /// 输出调试日志（-v 为 DEBUG，-vv 为 TRACE）
    #[arg(short, long, alias = "debug", action = ArgAction::Count, global = true)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Default, Args)]
pub struct StudyOptions {
    /// 使用 cookies.txt 中保存的 cookies 登录
    #[arg(long)]
    pub use_cookies: bool,

    /// 手机号账号
    #[arg(short, long)]
    pub username: Option<String>,

    /// 登录密码
    #[arg(short, long)]
    pub password: Option<String>,

    /// 要学习的课程 ID 列表，以 , 分隔
    #[arg(long = "course-ids", value_name = "IDS")]
    pub course_ids: Option<String>,

    /// 视频播放倍速（默认 1，最大 2）
    #[arg(short, long)]
    pub speed: Option<f64>,

    /// 同时进行的章节数（默认 4，一个章节内的多个任务点按顺序处理）
    #[arg(short, long)]
    pub jobs: Option<usize>,

    /// 遇到未开放章节时的行为
    #[arg(short = 'a', long, value_enum)]
    pub notopen_action: Option<NotOpenAction>,

    /// 任务失败/未开放章节的重试等待时间，单位秒（默认 1.0）
    #[arg(long)]
    pub retry_interval: Option<f64>,

    /// 是否启用章节检测答题（true/false，默认 true；覆盖配置文件）
    #[arg(long, action = ArgAction::Set)]
    pub work_enabled: Option<bool>,

    /// 视频等任务运行时，提前在后台查询后续章节检测答案（true/false；覆盖配置文件）
    #[arg(long, action = ArgAction::Set)]
    pub work_prefetch_enabled: Option<bool>,

    /// 先尝试直接上报到结尾，服务器确认后立即完成（true/false；覆盖配置文件）
    #[arg(long, action = ArgAction::Set)]
    pub instant_complete: Option<bool>,

    /// 达到题库覆盖率后自动提交章节检测答案（true/false；覆盖配置文件）
    #[arg(long, action = ArgAction::Set)]
    pub submit: Option<bool>,

    /// 自动提交所需的最低题库覆盖率，范围 0～1（默认 0.8；覆盖配置文件）
    #[arg(long = "cover-rate", value_name = "RATE", value_parser = parse_cover_rate)]
    pub cover_rate: Option<f64>,

    /// 刷完任务点后增加章节学习次数
    #[arg(long = "add-learning-count")]
    pub add_learning_count: bool,

    /// 章节学习次数目标总次数（默认 100）
    #[arg(long = "target-count")]
    pub target_count: Option<u32>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 登录并执行所选课程的章节任务
    Study(StudyOptions),
    /// 使用配置好的题库/大模型回答一道题，用于测试题库与大模型配置是否可用
    Ask(AskArgs),
    /// 登录并列出课程
    Courses(CoursesArgs),
    /// 从 HAR 抓包中按顺序重放学习通音频/视频进度报文
    Replay(ReplayArgs),
}

#[derive(Debug, Args)]
pub struct CoursesArgs {
    /// 以 JSON 格式输出，供桌面界面使用
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AskArgs {
    /// 题目内容
    pub title: String,

    /// 题型
    #[arg(short = 't', long = "type", value_enum, default_value_t = AskType::Single)]
    pub qtype: AskType,

    /// 选项（可多次指定），例如 -o "A. 北京" -o "B. 上海"
    #[arg(short = 'o', long = "option", value_name = "OPTION")]
    pub options: Vec<String>,

    /// 课程名称（作为大模型的上下文）
    #[arg(long)]
    pub course: Option<String>,

    /// 允许使用本地答案缓存（默认绕过缓存，直接查询题库/大模型）
    #[arg(long)]
    pub use_cache: bool,
}

#[derive(Debug, Args)]
pub struct ReplayArgs {
    /// 浏览器或代理导出的 HAR/JSON/JSONL 抓包文件
    pub capture: PathBuf,

    /// 只重放指定媒体类型
    #[arg(long, value_enum, default_value_t = ReplayMedia::All)]
    pub media: ReplayMedia,

    /// 相邻报文之间的等待毫秒数（默认 2000）
    #[arg(long, default_value_t = 2000, value_name = "MS")]
    pub delay_ms: u64,

    /// 只打印将要发送的报文，不发请求
    #[arg(long)]
    pub dry_run: bool,

    /// 只重放指定 objectId
    #[arg(long)]
    pub object_id: Option<String>,

    /// 只重放指定 jobid
    #[arg(long)]
    pub jobid: Option<String>,

    /// 最多重放多少条
    #[arg(long)]
    pub limit: Option<usize>,

    /// 收到 isPassed=true 后立即停止
    #[arg(long, action = ArgAction::Set, default_value_t = true)]
    pub stop_on_passed: bool,

    /// 单条报文失败后继续发送后续报文
    #[arg(long)]
    pub continue_on_error: bool,

    /// 保留抓包中的 _t 时间戳（默认刷新为当前时间）
    #[arg(long)]
    pub keep_captured_t: bool,

    /// 优先使用抓包里的 Cookie，而不是 cookies.txt
    #[arg(long)]
    pub use_capture_cookies: bool,

    /// 使用配置文件中的账号密码重新登录并刷新 cookies.txt
    #[arg(long)]
    pub login: bool,

    /// cookies.txt 路径
    #[arg(long, default_value = "cookies.txt", value_name = "FILE")]
    pub cookies: PathBuf,

    /// 重放结束后服务器仍未确认完成，也返回成功
    #[arg(long)]
    pub allow_incomplete: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ReplayMedia {
    All,
    Audio,
    Video,
}

impl From<ReplayMedia> for crate::replay::MediaKind {
    fn from(value: ReplayMedia) -> Self {
        match value {
            ReplayMedia::All => Self::All,
            ReplayMedia::Audio => Self::Audio,
            ReplayMedia::Video => Self::Video,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AskType {
    Single,
    Multiple,
    Judgement,
    Completion,
    Short,
}

impl From<AskType> for QuestionType {
    fn from(value: AskType) -> Self {
        match value {
            AskType::Single => QuestionType::Single,
            AskType::Multiple => QuestionType::Multiple,
            AskType::Judgement => QuestionType::Judgement,
            AskType::Completion => QuestionType::Completion,
            AskType::Short => QuestionType::ShortAnswer,
        }
    }
}

impl Cli {
    /// 合并配置文件与命令行参数，得到最终配置。
    pub fn build_config(&self) -> anyhow::Result<AppConfig> {
        let mut cfg = AppConfig::from_file(&self.config)?;
        if let Command::Study(options) = &self.command {
            options.apply_overrides(&mut cfg.common);
            options.apply_tiku_overrides(&mut cfg.tiku);
        }
        cfg.common.normalize();
        Ok(cfg)
    }
}

fn parse_cover_rate(value: &str) -> Result<f64, String> {
    let rate: f64 = value
        .trim()
        .parse()
        .map_err(|_| "题库覆盖率必须是数字".to_owned())?;
    if !rate.is_finite() || !(0.0..=1.0).contains(&rate) {
        return Err("题库覆盖率须在 0 到 1 之间".to_owned());
    }
    Ok(rate)
}

impl StudyOptions {
    fn apply_overrides(&self, common: &mut CommonConfig) {
        if self.use_cookies {
            common.use_cookies = true;
        }
        if let Some(v) = &self.username {
            common.username = v.trim().to_owned();
        }
        if let Some(v) = &self.password {
            common.password = v.trim().to_owned();
        }
        if let Some(v) = &self.course_ids {
            common.course_list = split_list(v);
        }
        if let Some(v) = self.speed {
            common.speed = v;
        }
        if let Some(v) = self.jobs {
            common.jobs = v;
        }
        if let Some(v) = self.notopen_action {
            common.notopen_action = v;
        }
        if let Some(v) = self.retry_interval {
            common.retry_interval = v;
        }
        if let Some(v) = self.work_enabled {
            common.work_enabled = v;
        }
        if let Some(v) = self.work_prefetch_enabled {
            common.work_prefetch_enabled = v;
        }
        if let Some(v) = self.instant_complete {
            common.instant_complete = v;
        }
        if self.add_learning_count {
            common.add_learning_count = true;
        }
        if let Some(v) = self.target_count {
            common.target_count = v;
        }
    }

    fn apply_tiku_overrides(&self, tiku: &mut Section) {
        if let Some(submit) = self.submit {
            tiku.set("submit", if submit { "true" } else { "false" });
        }
        if let Some(rate) = self.cover_rate {
            tiku.set("cover_rate", rate.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::parse_from(args)
    }

    #[test]
    fn cli_overrides_config_values() {
        let cli = parse(&["chaoxing", "study", "-s", "5", "-j", "2", "--debug"]);
        let mut common = CommonConfig::default();
        let Command::Study(options) = cli.command else {
            panic!("expected study")
        };
        options.apply_overrides(&mut common);
        common.normalize();
        assert_eq!(common.speed, 2.0);
        assert_eq!(common.jobs, 2);
        assert_eq!(cli.verbose, 1);
    }

    #[test]
    fn work_enabled_override_supports_both_directions_and_preserves_config_when_omitted() {
        for (args, initial, expected) in [
            (
                vec!["chaoxing", "study", "--work-enabled", "false"],
                true,
                false,
            ),
            (
                vec!["chaoxing", "study", "--work-enabled", "true"],
                false,
                true,
            ),
            (vec!["chaoxing", "study"], false, false),
            (vec!["chaoxing", "study"], true, true),
        ] {
            let cli = parse(&args);
            let Command::Study(options) = cli.command else {
                panic!("expected study")
            };
            let mut common = CommonConfig {
                work_enabled: initial,
                ..Default::default()
            };
            options.apply_overrides(&mut common);
            assert_eq!(common.work_enabled, expected);
        }
        assert!(Cli::try_parse_from(["chaoxing", "study", "--work-enabled", "maybe"]).is_err());
    }

    #[test]
    fn work_prefetch_override_supports_both_directions() {
        for (args, initial, expected) in [
            (
                vec!["chaoxing", "study", "--work-prefetch-enabled", "true"],
                false,
                true,
            ),
            (
                vec!["chaoxing", "study", "--work-prefetch-enabled", "false"],
                true,
                false,
            ),
            (vec!["chaoxing", "study"], true, true),
        ] {
            let cli = parse(&args);
            let Command::Study(options) = cli.command else {
                panic!("expected study")
            };
            let mut common = CommonConfig {
                work_prefetch_enabled: initial,
                ..Default::default()
            };
            options.apply_overrides(&mut common);
            assert_eq!(common.work_prefetch_enabled, expected);
        }
    }

    #[test]
    fn submit_and_cover_rate_override_tiku_section() {
        for (args, submit, rate) in [
            (
                vec![
                    "chaoxing",
                    "study",
                    "--submit",
                    "true",
                    "--cover-rate",
                    "0.9",
                ],
                Some("true"),
                Some("0.9"),
            ),
            (
                vec!["chaoxing", "study", "--submit", "false"],
                Some("false"),
                None,
            ),
        ] {
            let cli = parse(&args);
            let Command::Study(options) = cli.command else {
                panic!("expected study")
            };
            let mut tiku = Section::default();
            tiku.set("submit", "true");
            tiku.set("cover_rate", "0.5");
            options.apply_tiku_overrides(&mut tiku);
            assert_eq!(tiku.get("submit"), submit.or(Some("true")));
            assert_eq!(tiku.get("cover_rate"), rate.or(Some("0.5")));
        }
    }

    #[test]
    fn cover_rate_rejects_values_outside_zero_to_one() {
        for value in ["-0.1", "1.1", "abc", "NaN"] {
            assert!(
                Cli::try_parse_from(["chaoxing", "study", "--cover-rate", value]).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn ask_subcommand_accepts_global_config() {
        let cli = parse(&[
            "chaoxing", "ask", "题目", "-t", "multiple", "-o", "A. 1", "-o", "B. 2", "-c", "x.ini",
        ]);
        assert_eq!(cli.config.as_path(), std::path::Path::new("x.ini"));
        let Command::Ask(ask) = cli.command else {
            panic!("expected ask")
        };
        assert_eq!(ask.options.len(), 2);
        assert!(matches!(ask.qtype, AskType::Multiple));
    }

    #[test]
    fn courses_subcommand_supports_machine_readable_output() {
        let cli = parse(&["chaoxing", "courses", "--json", "-c", "x.ini"]);
        assert_eq!(cli.config.as_path(), std::path::Path::new("x.ini"));
        assert!(matches!(
            cli.command,
            Command::Courses(CoursesArgs { json: true })
        ));
    }

    #[test]
    fn global_config_can_precede_or_follow_study() {
        for args in [
            [
                "chaoxing",
                "-c",
                "config.ini",
                "study",
                "--course-ids",
                "1,2",
            ],
            [
                "chaoxing",
                "study",
                "-c",
                "config.ini",
                "--course-ids",
                "1,2",
            ],
        ] {
            let cli = parse(&args);
            assert_eq!(cli.config.as_path(), std::path::Path::new("config.ini"));
            let Command::Study(options) = cli.command else {
                panic!("expected study")
            };
            assert_eq!(options.course_ids.as_deref(), Some("1,2"));
        }
    }
}
