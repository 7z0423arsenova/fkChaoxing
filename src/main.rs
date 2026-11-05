mod captcha;
mod cli;
mod client;
mod config;
mod console;
mod crypto;
mod decode;
mod error;
mod font;
mod fsutil;
mod http;
mod llm;
mod logging;
mod models;
mod ocr;
mod replay;
mod runner;
mod similarity;
mod study;
mod tiku;
mod ui;

#[cfg(test)]
mod testutil;

use std::sync::Arc;

use anyhow::{Result, bail};
use clap::Parser;
use cli::{Cli, Command, ReplayArgs};
use client::ChaoxingClient;
use config::{AppConfig, CommonConfig};
use models::Question;
use tiku::{QueryContext, Tiku};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    logging::init(cli.verbose);
    if let Err(error) = run(cli).await {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    if let Command::Replay(args) = &cli.command {
        return replay_command(&cli, args).await;
    }
    let config = cli.build_config()?;
    let captcha = captcha::CaptchaConfig::from_section(&config.captcha).build();
    match cli.command {
        Command::Ask(args) => {
            let http = Arc::new(http::Http::new("cookies.txt")?);
            let mut tiku = Tiku::from_config(&config.tiku, &http).await?;
            if tiku.is_disabled() {
                bail!("未配置可用题库；请在配置文件的 [tiku] 中设置 provider");
            }
            if !args.use_cache {
                tiku = tiku.without_cache();
            }
            let mut questions = [Question::new(args.title, args.qtype.into(), &args.options)];
            let context = QueryContext {
                course: args.course.as_deref(),
                ..Default::default()
            };
            let answers = tiku.query_all(&mut questions, &context).await;
            match answers.into_iter().next().flatten() {
                Some(answer) => println!("{answer}"),
                None => bail!("题库未找到可用答案"),
            }
        }
        Command::Courses(args) => {
            let http = Arc::new(http::Http::new("cookies.txt")?);
            let client = ChaoxingClient::new(http).with_captcha(captcha);
            login_client(&client, &config.common).await?;
            let courses = client.courses().await?;
            if args.json {
                let data: Vec<_> = courses
                    .iter()
                    .map(|course| {
                        serde_json::json!({
                            "id": course.id,
                            "info": course.info,
                            "roleid": course.roleid,
                            "courseId": course.course_id,
                            "clazzId": course.clazz_id,
                            "cpi": course.cpi,
                            "title": course.title,
                            "teacher": course.teacher,
                            "desc": course.desc,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string(&data)?);
            } else {
                for course in courses {
                    println!(
                        "{}\t{}\t{}",
                        course.course_id, course.clazz_id, course.title
                    );
                }
            }
        }
        Command::Study(_) => {
            let http = Arc::new(http::Http::new("cookies.txt")?);
            let client = Arc::new(ChaoxingClient::new(http.clone()).with_captcha(captcha));
            login_client(&client, &config.common).await?;
            let tiku = Arc::new(if config.common.work_enabled {
                Tiku::from_config(&config.tiku, &http).await?
            } else {
                tracing::info!("已关闭章节检测答题，将跳过答题任务点");
                Tiku::disabled()
            });
            if config.common.work_enabled
                && config.tiku.bool_or("check_llm_connection", true)
                && !tiku.check_llm_connection().await
            {
                let continue_run = console::stdin_is_interactive()
                    && console::CONSOLE
                        .prompt("大模型连接检查失败，继续运行？[Y/n] ")
                        .await
                        .is_some_and(|answer| {
                            matches!(answer.trim().to_lowercase().as_str(), "" | "y" | "yes")
                        });
                if !continue_run {
                    bail!("大模型连接检查失败");
                }
            }
            let query_delay = config.tiku.get_f64("delay").unwrap_or(0.0).max(0.0);
            runner::run(client, tiku, &config.common, query_delay).await?;
        }
        Command::Replay(_) => unreachable!("replay 已在 build_config 之前处理"),
    }
    Ok(())
}

async fn replay_command(cli: &Cli, args: &ReplayArgs) -> Result<()> {
    let http = Arc::new(http::Http::new(args.cookies.clone())?);
    if args.login {
        let config = AppConfig::from_file(&cli.config)?;
        let captcha = captcha::CaptchaConfig::from_section(&config.captcha).build();
        let client = ChaoxingClient::new(http.clone()).with_captcha(captcha);
        login_client(&client, &config.common).await?;
    }

    let capture = replay::load_capture(&args.capture)?;
    let options = replay::ReplayOptions {
        media: args.media.into(),
        object_id: args.object_id.clone(),
        jobid: args.jobid.clone(),
        limit: args.limit,
        delay: std::time::Duration::from_millis(args.delay_ms),
        dry_run: args.dry_run,
        stop_on_passed: args.stop_on_passed,
        continue_on_error: args.continue_on_error,
        refresh_t: !args.keep_captured_t,
        cookie: None,
        prefer_capture_cookies: args.use_capture_cookies,
    };
    let selected = replay::select(&capture, &options);
    if selected.is_empty() {
        bail!(
            "抓包里没有匹配的媒体进度报文；可用 --media all 检查，或确认抓的是 /mooc-ans/multimedia/log/ 请求"
        );
    }
    println!(
        "抓包：{}，匹配媒体进度报文 {} 条",
        args.capture.display(),
        selected.len()
    );
    for request in &selected {
        if let Some(warning) = replay::enc_mismatch(request) {
            tracing::warn!("{warning}");
        }
    }
    if !args.dry_run
        && http.uid().is_err()
        && selected
            .iter()
            .all(|request| request.header("cookie").is_none())
    {
        bail!(
            "没有可用 Cookie：请先导出带 Cookie 的抓包，或用 --login 重新登录，或通过 --cookies 指定 cookies.txt"
        );
    }

    let outcome = replay::replay(&http, &selected, &options).await?;
    if args.dry_run {
        println!("dry-run：共 {} 条，未发送请求。", outcome.sent);
        return Ok(());
    }
    println!(
        "已重放 {} 条报文；服务器确认完成：{}",
        outcome.sent,
        if outcome.passed { "是" } else { "否" }
    );
    if !outcome.passed && !args.allow_incomplete {
        bail!(
            "服务器尚未确认任务完成；可能抓包没有覆盖到结尾，或需要保留更长的发送间隔（--delay-ms）"
        );
    }
    Ok(())
}

async fn login_client(client: &ChaoxingClient, common: &CommonConfig) -> Result<()> {
    let mut username = common.username.clone();
    let mut password = common.password.clone();
    if !common.use_cookies && console::stdin_is_interactive() {
        if username.is_empty() {
            username = console::CONSOLE
                .prompt("手机号: ")
                .await
                .unwrap_or_default();
        }
        if password.is_empty() {
            password = console::CONSOLE.prompt("密码: ").await.unwrap_or_default();
        }
    }
    client.login(common.use_cookies, &username, &password).await
}
