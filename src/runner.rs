//! 课程与章节任务的并发调度。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures::{StreamExt, stream};
use tracing::{error, info, warn};

use crate::client::ChaoxingClient;
use crate::config::{CommonConfig, NotOpenAction, split_list};
use crate::console::{CONSOLE, stdin_is_interactive};
use crate::error::VerificationRequired;
use crate::models::{Course, Job, JobInfo, JobType, Point};
use crate::study;
use crate::tiku::Tiku;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChapterOutcome {
    Done,
    Closed,
}

pub async fn run(
    client: Arc<ChaoxingClient>,
    tiku: Arc<Tiku>,
    config: &CommonConfig,
    query_delay: f64,
) -> Result<()> {
    let courses = client.courses().await?;
    let mut selected_ids = config.course_list.clone();
    if selected_ids.is_empty() && stdin_is_interactive() {
        for course in &courses {
            println!(
                "ID: {} 班级ID: {} 课程名: {}",
                course.course_id, course.clazz_id, course.title
            );
        }
        if let Some(line) = CONSOLE
            .prompt("请输入想要学习的课程 ID（逗号分隔，留空学习全部）：")
            .await
        {
            selected_ids = split_list(&line);
        }
    }
    let selected = filter_courses(courses, &selected_ids);
    if selected.is_empty() {
        bail!("没有找到可学习的课程");
    }
    let mut tasks = Vec::new();
    for course in &selected {
        let points = client
            .course_points(course)
            .await
            .context("读取课程章节失败")?;
        crate::ui::course(&course.title, &course.clazz_id, points.points.len());
        for point in points.points {
            tasks.push((course.clone(), point));
        }
    }
    info!("共读取到 {} 个章节任务", tasks.len());
    let mut running = stream::iter(tasks.into_iter().map(|(course, point)| {
        let client = client.clone();
        let tiku = tiku.clone();
        let config = config.clone();
        async move {
            let result = retry_chapter(&client, &tiku, &course, &point, &config, query_delay).await;
            (course.title, point.title, result)
        }
    }))
    .buffer_unordered(config.jobs.max(1));
    let mut failed = 0;
    while let Some((course, point, result)) = running.next().await {
        match result {
            Ok(()) => info!("完成章节：{course} / {point}"),
            Err(err) => {
                failed += 1;
                error!("章节失败：{course} / {point}: {err:#}");
                if err.downcast_ref::<VerificationRequired>().is_some() {
                    warn!(
                        "章节 {course} / {point} 触发验证码/安全验证，跳过该章节并继续其他章节；若已开启 [captcha] enabled=true，请查看日志中的 ddddocr 错误"
                    );
                }
            }
        }
    }
    if failed > 0 {
        bail!("有 {failed} 个章节未完成，请查看日志");
    }
    info!("所有课程学习任务已完成");
    if config.add_learning_count {
        for course in &selected {
            let points = client.course_points(course).await?;
            study::increase_learning_count(&client, course, &points.points, config.target_count)
                .await?;
        }
    }
    Ok(())
}

fn filter_courses(courses: Vec<Course>, selected_ids: &[String]) -> Vec<Course> {
    let mut seen = HashSet::new();
    courses
        .into_iter()
        .filter(|course| {
            (selected_ids.is_empty() || selected_ids.contains(&course.course_id))
                && seen.insert((course.course_id.clone(), course.clazz_id.clone()))
        })
        .collect()
}

async fn retry_chapter(
    client: &Arc<ChaoxingClient>,
    tiku: &Arc<Tiku>,
    course: &Course,
    point: &Point,
    config: &CommonConfig,
    query_delay: f64,
) -> Result<()> {
    for attempt in 1..=5 {
        match process_chapter(client, tiku, course, point, config, query_delay).await {
            Ok(ChapterOutcome::Done) => {
                crate::ui::chapter(&course.title, &course.clazz_id, &point.title, "done");
                return Ok(());
            }
            Ok(ChapterOutcome::Closed) if config.notopen_action == NotOpenAction::Continue => {
                warn!("章节未开放，跳过：{} / {}", course.title, point.title);
                crate::ui::chapter(&course.title, &course.clazz_id, &point.title, "closed");
                return Ok(());
            }
            Ok(ChapterOutcome::Closed)
                if config.notopen_action == NotOpenAction::Ask && stdin_is_interactive() =>
            {
                let message = format!(
                    "章节 {} / {} 未开放，跳过吗？[y/N] ",
                    course.title, point.title
                );
                if CONSOLE.prompt(message).await.is_some_and(|answer| {
                    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes" | "是")
                }) {
                    return Ok(());
                }
            }
            Ok(ChapterOutcome::Closed) => warn!(
                "章节未开放：{} / {} ({attempt}/5)",
                course.title, point.title
            ),
            Err(error) if error.downcast_ref::<VerificationRequired>().is_some() => {
                warn!(
                    "章节 {} / {} 触发验证码/安全验证；请查看上一条“学习通返回验证页面”日志确认具体接口",
                    course.title, point.title
                );
                return Err(error);
            }
            Err(error)
                if error.downcast_ref::<study::MediaNotPassed>().is_some()
                    || error.downcast_ref::<study::LiveNotFinished>().is_some() =>
            {
                return Err(error);
            }
            Err(error) => warn!(
                "章节处理失败：{} / {} ({attempt}/5): {error:#}",
                course.title, point.title
            ),
        }
        if attempt < 5 {
            tokio::time::sleep(Duration::from_secs_f64(config.retry_interval.max(0.0))).await;
        }
    }
    crate::ui::chapter(&course.title, &course.clazz_id, &point.title, "failed");
    bail!("章节连续 5 次未完成")
}

async fn process_chapter(
    client: &Arc<ChaoxingClient>,
    tiku: &Arc<Tiku>,
    course: &Course,
    point: &Point,
    config: &CommonConfig,
    query_delay: f64,
) -> Result<ChapterOutcome> {
    if point.has_finished {
        crate::ui::chapter(&course.title, &course.clazz_id, &point.title, "done");
        return Ok(ChapterOutcome::Done);
    }
    crate::ui::chapter(&course.title, &course.clazz_id, &point.title, "start");
    let (jobs, info) = client.chapter_jobs(course, point).await?;
    tracing::debug!("章节 {} 声明任务数: {}", point.title, point.job_count);
    if info.not_open {
        crate::ui::chapter(&course.title, &course.clazz_id, &point.title, "closed");
        return Ok(ChapterOutcome::Closed);
    }
    if jobs.is_empty() {
        study::study_empty_page(client, course, point).await?;
    }
    let mut prefetch = PrefetchTask::start(
        client,
        tiku,
        course,
        point,
        &jobs,
        &info,
        config,
        query_delay,
    );
    for job in &jobs {
        if job.kind == JobType::Work && !config.work_enabled {
            info!(
                "已关闭答题，跳过章节检测：{} / {} / {}",
                course.title,
                point.title,
                job.display_name()
            );
            continue;
        }
        info!(
            "开始任务：{} / {} / {}",
            course.title,
            point.title,
            job.display_name()
        );
        match job.kind {
            JobType::Video => {
                study::study_video(client, course, job, config.speed, config.instant_complete)
                    .await?
            }
            JobType::Document => study::study_document(client, course, job).await?,
            JobType::Work => {
                prefetch.wait().await;
                study::study_work(
                    client,
                    course,
                    job,
                    &point.title,
                    &info,
                    tiku,
                    config.work_redo_enabled,
                    config.work_max_retries,
                    query_delay,
                )
                .await?
            }
            JobType::Read => study::study_read(client, course, job, &info).await?,
            JobType::Live => study::study_live(client, course, job, &info, config.speed).await?,
        }
    }
    confirm_chapter_finished(client, course, point).await?;
    Ok(ChapterOutcome::Done)
}

/// 只有章节检测前面还有别的任务时才值得预取，否则没有并行收益。
fn should_prefetch(config: &CommonConfig, tiku: &Tiku, jobs: &[Job]) -> bool {
    if !config.work_enabled
        || !config.work_prefetch_enabled
        || tiku.is_disabled()
        || tiku.is_manual()
    {
        return false;
    }
    let Some(first_work) = jobs.iter().position(|job| job.kind == JobType::Work) else {
        return false;
    };
    jobs[..first_work]
        .iter()
        .any(|job| job.kind != JobType::Work)
}

/// 后台预取任务。章节检测开始前会等待它完成；章节提前失败时在 Drop 中取消。
struct PrefetchTask(Option<tokio::task::JoinHandle<()>>);

impl PrefetchTask {
    fn start(
        client: &Arc<ChaoxingClient>,
        tiku: &Arc<Tiku>,
        course: &Course,
        point: &Point,
        jobs: &[Job],
        info: &JobInfo,
        config: &CommonConfig,
        query_delay: f64,
    ) -> Self {
        if !should_prefetch(config, tiku, jobs) {
            return Self(None);
        }
        let client = client.clone();
        let tiku = tiku.clone();
        let course = course.clone();
        let chapter = point.title.clone();
        let info = info.clone();
        let jobs = jobs.to_vec();
        Self(Some(tokio::spawn(study::prefetch_work(
            client,
            tiku,
            course,
            chapter,
            info,
            jobs,
            query_delay,
        ))))
    }

    async fn wait(&mut self) {
        if let Some(handle) = self.0.take()
            && let Err(error) = handle.await
        {
            tracing::debug!("后台预取任务异常结束: {error}");
        }
    }
}

impl Drop for PrefetchTask {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// 学习通的章节完成状态可能延迟更新。这里只做提示，不把“暂时未标记完成”当成失败：
/// 只要所有任务点返回成功，就视为本章处理完成。
async fn confirm_chapter_finished(
    client: &ChaoxingClient,
    course: &Course,
    point: &Point,
) -> Result<()> {
    for delay in [0, 1, 2] {
        if delay > 0 {
            tokio::time::sleep(Duration::from_secs(delay)).await;
        }
        let points = match client.course_points(course).await {
            Ok(points) => points,
            Err(error) if error.downcast_ref::<VerificationRequired>().is_some() => {
                return Err(error);
            }
            Err(error) => {
                return Err(error.context(format!(
                    "读取章节完成状态失败：{} / {}",
                    course.title, point.title
                )));
            }
        };
        let Some(current) = points
            .points
            .iter()
            .find(|candidate| candidate.id == point.id)
        else {
            bail!(
                "课程页面没有返回章节 {}，无法确认完成状态：{} / {}",
                point.id,
                course.title,
                point.title
            );
        };
        if current.has_finished {
            return Ok(());
        }
    }
    bail!(
        "服务器未确认章节完成，已按未完成处理：{} / {}",
        course.title,
        point.title
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Endpoints;
    use crate::http::Http;
    use crate::testutil::{MockResponse, MockServer};

    #[test]
    fn filtering_preserves_multiple_classes_of_same_course_and_removes_duplicates() {
        let course = |id: &str, class: &str| Course {
            course_id: id.into(),
            clazz_id: class.into(),
            ..Default::default()
        };
        let result = filter_courses(
            vec![
                course("1", "10"),
                course("1", "20"),
                course("1", "10"),
                course("2", "30"),
            ],
            &["1".into()],
        );
        assert_eq!(result.len(), 2);
        assert_eq!(result[1].clazz_id, "20");
    }

    #[tokio::test]
    async fn disabling_work_skips_work_requests_but_still_processes_documents() {
        let text = |body: &str| MockResponse {
            status: 200,
            body: body.into(),
            headers: vec![],
        };
        let mut responses = vec![text(
            r#"<script>mArg = {"attachments":[
            {"type":"workid","job":true,"jobid":"work-55","enc":"e1"},
            {"type":"document","job":true,"jobid":"d1","otherInfo":"nodeId_1001-cpi_30","jtoken":"tk"}
        ],"defaults":{"knowledgeid":"1001","ktoken":"kt","cpi":"30"}};</script>"#,
        )];
        responses.push(text(""));
        responses.push(text("ok"));
        responses.push(text(r#"<div class="chapter_unit"><ul><li><div id="cur1001"><a class="clicktitle">第一章</a><span class="bntHoverTips">已完成</span></div></li></ul></div>"#));
        let server = MockServer::start(responses).await;
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        let client = Arc::new(ChaoxingClient::new_with_endpoints(
            http.clone(),
            Endpoints::all(&server.url),
        ));
        let ini = crate::config::Ini::parse("[tiku]\nprovider = TikuManual\n");
        let tiku = Arc::new(
            Tiku::from_config(ini.section("tiku").unwrap(), &http)
                .await
                .unwrap(),
        );
        assert!(!tiku.is_disabled());
        let outcome = process_chapter(
            &client,
            &tiku,
            &Course {
                course_id: "10".into(),
                clazz_id: "20".into(),
                cpi: "30".into(),
                ..Default::default()
            },
            &Point {
                id: "1001".into(),
                ..Default::default()
            },
            &CommonConfig {
                work_enabled: false,
                ..Default::default()
            },
            0.0,
        )
        .await
        .unwrap();
        assert_eq!(outcome, ChapterOutcome::Done);
        let requests = server.requests();
        assert_eq!(requests.len(), 4);
        assert!(requests[2].path.starts_with("/ananas/job/document?"));
        assert!(
            requests
                .iter()
                .all(|request| !request.path.contains("work"))
        );
    }

    #[tokio::test]
    async fn unconfirmed_chapter_is_an_error() {
        let responses = vec![
            MockResponse {
                status: 200,
                body: r#"<script>mArg = {"attachments":[],"defaults":{"knowledgeid":"1001","cpi":"30"}};</script>"#.into(),
                headers: vec![],
            },
            MockResponse {
                status: 200,
                body: String::new(),
                headers: vec![],
            },
            MockResponse {
                status: 200,
                body: String::new(),
                headers: vec![],
            },
            MockResponse {
                status: 200,
                body: r#"<div class="chapter_unit"><ul><li><div id="cur1001"><a class="clicktitle">第一章</a><span class="bntHoverTips">未完成</span><input class="knowledgeJobCount" value="1"></div></li></ul></div>"#.into(),
                headers: vec![],
            },
        ];
        let server = MockServer::start(responses).await;
        let client = Arc::new(ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        ));
        let tiku = Arc::new(Tiku::disabled());
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let point = Point {
            id: "1001".into(),
            title: "第一章".into(),
            ..Default::default()
        };
        let error = process_chapter(
            &client,
            &tiku,
            &course,
            &point,
            &CommonConfig::default(),
            0.0,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("未确认"), "{error:#}");
    }

    #[tokio::test]
    async fn chapter_is_done_when_course_page_confirms_completion() {
        let responses = vec![
            MockResponse {
                status: 200,
                body: r#"<script>mArg = {"attachments":[],"defaults":{"knowledgeid":"1001","cpi":"30"}};</script>"#.into(),
                headers: vec![],
            },
            MockResponse {
                status: 200,
                body: String::new(),
                headers: vec![],
            },
            MockResponse {
                status: 200,
                body: String::new(),
                headers: vec![],
            },
            MockResponse {
                status: 200,
                body: r#"<div class="chapter_unit"><ul><li><div id="cur1001"><a class="clicktitle">第一章</a><span class="bntHoverTips">已完成</span><input class="knowledgeJobCount" value="1"></div></li></ul></div>"#.into(),
                headers: vec![],
            },
        ];
        let server = MockServer::start(responses).await;
        let client = Arc::new(ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        ));
        let tiku = Arc::new(Tiku::disabled());
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let point = Point {
            id: "1001".into(),
            title: "第一章".into(),
            ..Default::default()
        };
        let outcome = process_chapter(
            &client,
            &tiku,
            &course,
            &point,
            &CommonConfig::default(),
            0.0,
        )
        .await
        .unwrap();
        assert_eq!(outcome, ChapterOutcome::Done);
    }

    #[tokio::test]
    async fn prefetch_only_starts_when_media_precedes_work() {
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        let ini = crate::config::Ini::parse("[tiku]\nprovider = TikuGo\n");
        let tiku = Tiku::from_config(ini.section("tiku").unwrap(), &http)
            .await
            .unwrap();
        let config = CommonConfig {
            work_prefetch_enabled: true,
            ..Default::default()
        };
        let media_then_work = [Job::new(JobType::Video), Job::new(JobType::Work)];
        assert!(should_prefetch(&config, &tiku, &media_then_work));

        let work_first = [Job::new(JobType::Work), Job::new(JobType::Video)];
        assert!(!should_prefetch(&config, &tiku, &work_first));
        assert!(!should_prefetch(
            &CommonConfig::default(),
            &tiku,
            &media_then_work
        ));
        let work_disabled = CommonConfig {
            work_enabled: false,
            work_prefetch_enabled: true,
            ..Default::default()
        };
        assert!(!should_prefetch(&work_disabled, &tiku, &media_then_work));
        assert!(!should_prefetch(
            &config,
            &Tiku::disabled(),
            &media_then_work
        ));

        let ini = crate::config::Ini::parse("[tiku]\nprovider = TikuManual\n");
        let manual = Tiku::from_config(ini.section("tiku").unwrap(), &http)
            .await
            .unwrap();
        assert!(!should_prefetch(&config, &manual, &media_then_work));
    }
}
