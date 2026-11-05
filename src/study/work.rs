//! 章节检测：获取题目、查询题库、保存或提交答案。

use anyhow::{Context, Result, bail};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use scraper::{Html, Selector};
use serde_json::Value;
use tracing::warn;

use crate::client::ChaoxingClient;
use crate::decode::{WorkForm, parse_work_form};
use crate::http::verification_required;
use crate::models::{Course, Job, JobInfo, JobType};
use crate::tiku::{QueryContext, Tiku, matching};

use super::VerificationRequired;

pub async fn study_work(
    client: &ChaoxingClient,
    course: &Course,
    job: &Job,
    chapter: &str,
    info: &JobInfo,
    tiku: &Tiku,
    redo: bool,
    max_retries: u32,
    query_delay: f64,
) -> Result<()> {
    if tiku.is_disabled() {
        return Ok(());
    }
    let mut feedback = Vec::new();
    let rounds = if redo {
        max_retries.max(1).min(10) + 1
    } else {
        1
    };
    for round in 0..rounds {
        let submission = submit_work_round(
            client,
            course,
            job,
            chapter,
            info,
            tiku,
            &feedback,
            query_delay,
            round > 0,
        )
        .await?;
        let Some(submission) = submission else {
            return Ok(());
        };
        if !submission.submitted {
            return Ok(());
        }
        match check_work_result(client, course, job, info, &submission).await {
            Ok(Some(result)) => {
                if result.score >= 99.95 || result.all_correct {
                    return Ok(());
                }
                if result.unknown > 0 {
                    warn!(
                        "章节检测成绩 {} 分，有 {} 题无法解析答案详情",
                        result.score, result.unknown
                    );
                }
                if round + 1 >= rounds {
                    warn!("章节检测成绩 {} 分，保留本次作答；请人工核对", result.score);
                    return Ok(());
                }
                feedback = result.feedback;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Ok(None) => {
                warn!("无法读取章节检测成绩详情");
                if round + 1 >= rounds {
                    warn!("保留本次作答；请人工核对");
                    return Ok(());
                }
                feedback.clear();
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(error) => {
                // 验证码会中断整轮任务，必须交给上层处理，不能当成“暂时读不到成绩”。
                if error.downcast_ref::<VerificationRequired>().is_some() {
                    return Err(error);
                }
                warn!("获取章节检测成绩失败: {error:#}");
                if round + 1 >= rounds {
                    warn!("保留本次作答；请人工核对");
                    return Ok(());
                }
                feedback.clear();
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    Ok(())
}

/// 在视频等任务进行时，提前查询后续章节检测答案并写入本地缓存。
///
/// 这里只负责查题，不提交；真正提交仍由 `study_work` 完成。预取失败会被忽略，
/// 到章节检测时主流程会重新读取页面并查询。
pub async fn prefetch_work(
    client: Arc<ChaoxingClient>,
    tiku: Arc<Tiku>,
    course: Course,
    chapter: String,
    info: JobInfo,
    jobs: Vec<Job>,
    query_delay: f64,
) {
    for job in jobs.iter().filter(|job| job.kind == JobType::Work) {
        let mut form = match fetch_work_form(&client, &course, job, &info).await {
            Ok(Some(form)) => form,
            Ok(None) => {
                tracing::debug!(
                    "后台预取跳过：教师未创建完成该测验（{} / {}）",
                    course.title,
                    chapter
                );
                continue;
            }
            Err(error) => {
                tracing::debug!(
                    "后台预取失败，章节检测时会重新读取：{} / {} / {}: {error:#}",
                    course.title,
                    chapter,
                    job.display_name()
                );
                continue;
            }
        };
        let count = form.questions.len();
        if count == 0 {
            continue;
        }
        let answers = tiku
            .query_all(
                &mut form.questions,
                &QueryContext {
                    course: Some(&course.title),
                    delay: query_delay,
                    ..Default::default()
                },
            )
            .await;
        let covered = answers.iter().filter(|answer| answer.is_some()).count();
        tracing::info!(
            "后台预取章节检测答案：{} / {} / {}（{covered}/{count}）",
            course.title,
            chapter,
            job.display_name()
        );
    }
}

async fn fetch_work_form(
    client: &ChaoxingClient,
    course: &Course,
    job: &Job,
    info: &JobInfo,
) -> Result<Option<WorkForm>> {
    let url = format!(
        "{}/mooc-ans/api/work",
        client.endpoints.mooc1.trim_end_matches('/')
    );
    let work_id = job.jobid.strip_prefix("work-").unwrap_or(&job.jobid);
    let mut form = None;
    for attempt in 0..3 {
        let request = client.http.client.get(&url).query(&[
            ("api", "1"),
            ("workId", work_id),
            ("jobid", &job.jobid),
            ("originJobId", &job.jobid),
            ("needRedirect", "true"),
            ("skipHeader", "true"),
            ("knowledgeid", &info.knowledgeid),
            ("ktoken", &info.ktoken),
            ("cpi", &info.cpi),
            ("ut", "s"),
            ("clazzId", &course.clazz_id),
            ("type", ""),
            ("enc", &job.enc),
            ("mooc2", "1"),
            ("courseid", &course.course_id),
        ]);
        let fetched = client.fetch(request).await.context("读取章节检测失败")?;
        let ok = fetched.status.is_success();
        let response_url = fetched.url;
        let html = fetched.body;
        // 验证码页面也会返回 200，不能把它当成“没有题目”继续重试。
        if let Some(error) = verification_required(&response_url, &html) {
            tracing::warn!(
                "章节检测触发验证：{} / job={} / workId={work_id}",
                course.title,
                job.jobid
            );
            return Err(error);
        }
        if html.contains("教师未创建完成该测验") {
            return Ok(None);
        }
        let decoded = parse_work_form(&html);
        if ok && !decoded.questions.is_empty() {
            form = Some(decoded);
            break;
        }
        if attempt < 2 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
    Ok(Some(form.context("章节检测未返回有效题目")?))
}

async fn submit_work_round(
    client: &ChaoxingClient,
    course: &Course,
    job: &Job,
    chapter: &str,
    info: &JobInfo,
    tiku: &Tiku,
    feedback: &[String],
    query_delay: f64,
    force_submit: bool,
) -> Result<Option<WorkSubmission>> {
    let work_id = job.jobid.strip_prefix("work-").unwrap_or(&job.jobid);
    let Some(mut form) = fetch_work_form(client, course, job, info).await? else {
        return Ok(None);
    };
    let count = form.questions.len();
    let answers = tiku
        .query_all(
            &mut form.questions,
            &QueryContext {
                course: Some(&course.title),
                feedback,
                delay: query_delay,
                ..Default::default()
            },
        )
        .await;
    let mut covered = 0;
    let mut resolved = Vec::with_capacity(count);
    for (question, answer) in form.questions.iter().zip(answers.iter()) {
        let value = answer
            .as_ref()
            .map(|answer| matching::resolve_answer(question, answer, &tiku.settings))
            .unwrap_or_default();
        if value.is_empty() {
            resolved.push((matching::random_answer(question), false));
        } else {
            covered += 1;
            resolved.push((value, true));
        }
    }
    let submit = tiku.settings.submit
        && (tiku.is_manual()
            || covered as f64 / count as f64 >= tiku.settings.cover_rate
            || force_submit);
    let source = tiku.label();
    for (index, (question, (value, hit))) in form.questions.iter().zip(resolved.iter()).enumerate()
    {
        crate::ui::answer(
            &course.title,
            &course.clazz_id,
            chapter,
            index + 1,
            count,
            question.q_type.display(),
            &question.title,
            if *hit { &source } else { "随机兜底" },
            *hit,
            value,
            submit,
        );
    }
    crate::ui::work(
        &course.title,
        &course.clazz_id,
        chapter,
        count,
        covered,
        submit,
    );
    populate_answers(&mut form, &resolved, submit);
    let submission = WorkSubmission {
        submitted: submit,
        work_id: form
            .get("workId")
            .or_else(|| form.get("workRelationId"))
            .filter(|id| !id.is_empty())
            .unwrap_or(work_id)
            .to_owned(),
        answer_id: form.get("workAnswerId").unwrap_or("").to_owned(),
    };
    let url = format!(
        "{}/mooc-ans/work/addStudentWorkNew",
        client.endpoints.mooc1.trim_end_matches('/')
    );
    let request = client
        .http
        .client
        .post(url)
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&form.fields);
    let fetched = client.fetch(request).await.context("保存章节检测失败")?;
    let status = fetched.status;
    let response_url = fetched.url;
    let body = fetched.body;
    if let Some(error) = verification_required(&response_url, &body) {
        tracing::warn!(
            "章节检测提交触发验证：{} / job={} / workId={work_id}",
            course.title,
            job.jobid
        );
        return Err(error);
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        tracing::warn!(
            "章节检测提交 403：{} / job={} / workId={work_id}",
            course.title,
            job.jobid
        );
        return Err(VerificationRequired.into());
    }
    if !status.is_success() {
        bail!("保存章节检测失败：HTTP {status}");
    }
    let data: Value = serde_json::from_str(&body).context("解析章节检测提交响应失败")?;
    if data.get("status").and_then(Value::as_bool) != Some(true) {
        bail!(
            "章节检测{}失败: {}",
            if submit { "提交" } else { "保存" },
            data.get("msg")
                .and_then(Value::as_str)
                .unwrap_or("未知错误")
        );
    }
    Ok(Some(submission))
}

struct WorkSubmission {
    submitted: bool,
    work_id: String,
    answer_id: String,
}

fn populate_answers(form: &mut WorkForm, answers: &[(String, bool)], submit: bool) {
    form.set("pyFlag", if submit { "" } else { "1" });
    let questions = form.questions.clone();
    for (question, (answer, covered)) in questions.iter().zip(answers) {
        let value = if submit || *covered {
            answer.as_str()
        } else {
            ""
        };
        form.set(format!("answer{}", question.id), value);
        form.set(format!("answertype{}", question.id), &question.type_code);
    }
}

#[derive(Debug)]
struct WorkResult {
    score: f64,
    all_correct: bool,
    unknown: usize,
    feedback: Vec<String>,
}

static RECORD_TIMES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"viewNum\s*"\s*>\s*第(\d+)次"#).unwrap());
static RECORD_SCORES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"viewScore\s*"\s*>\s*([\d.]+)分"#).unwrap());
static RESULT_QUESTIONS: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("div.singleQuesId").unwrap());
static RESULT_TIMU: LazyLock<Selector> = LazyLock::new(|| Selector::parse("div.TiMu").unwrap());
static MY_ANSWER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)我的答案[:：].*?class="[^"]*answerCon[^"]*"[^>]*>\s*(.*?)\s*</div>"#).unwrap()
});
static CORRECT_ANSWER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)正确答案[:：].*?class="[^"]*answerCon[^"]*"[^>]*>\s*(.*?)\s*</div>"#).unwrap()
});
static STRIP_TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
static QUESTION_TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)newZy_TItle">.*?</span>(.*?)</div>"#).unwrap());
static PAGE_SCORE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"本次成绩<i>([\d.]+)</i>分").unwrap());

/// 解析作答记录列表，返回 `(作答次数, 分数)`。
fn parse_work_records(html: &str) -> Vec<(u32, f64)> {
    let times: Vec<_> = RECORD_TIMES
        .captures_iter(html)
        .filter_map(|c| c[1].parse().ok())
        .collect();
    let scores: Vec<_> = RECORD_SCORES
        .captures_iter(html)
        .filter_map(|c| c[1].parse().ok())
        .collect();
    if times.len() != scores.len() {
        return times.into_iter().map(|time| (time, 0.0)).collect();
    }
    times.into_iter().zip(scores).collect()
}

/// 归一化判断题答案，统一“对/错”“true/false”等写法后再比较。
fn normalize_answer_text(answer: &str) -> String {
    let text: String = answer
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_uppercase();
    if ["TRUE", "T", "1", "对", "正确", "√", "是", "YES", "Y"].contains(&text.as_str()) {
        return "TRUE".into();
    }
    if [
        "FALSE",
        "F",
        "0",
        "错",
        "错误",
        "×",
        "X",
        "否",
        "NO",
        "N",
        "不对",
        "不正确",
    ]
    .contains(&text.as_str())
    {
        return "FALSE".into();
    }
    text.chars()
        .filter(|c| !"，。、！？；：,.!?;:()（）[]【】\"“”‘’-_ /\\|".contains(*c))
        .collect()
}

/// 解析作答详情页，收集错题反馈和无法解析的题目数。
fn parse_work_result_detail(html: &str, score: f64) -> Option<WorkResult> {
    let document = Html::parse_document(html);
    let mut questions: Vec<_> = document.select(&RESULT_QUESTIONS).collect();
    if questions.is_empty() {
        // 有些页面只有 div.TiMu，没有外层 singleQuesId。
        questions = document.select(&RESULT_TIMU).collect();
    }
    if questions.is_empty() {
        return None;
    }
    let mut feedback = Vec::new();
    let mut unknown = 0;
    for element in questions {
        let block = element.html();
        let block = block.as_str();
        let title = QUESTION_TITLE
            .captures(block)
            .map(|c| {
                STRIP_TAGS
                    .replace_all(&c[1], "")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let extract = |pattern: &Regex| {
            pattern
                .captures(block)
                .map(|c| STRIP_TAGS.replace_all(&c[1], "").trim().to_owned())
        };
        match (extract(&MY_ANSWER), extract(&CORRECT_ANSWER)) {
            (Some(mine), Some(correct)) => {
                if normalize_answer_text(&mine) != normalize_answer_text(&correct) {
                    feedback.push(format!(
                        "- 题目：{}\n  你的上次答案：{}\n  正确答案：{}",
                        title, mine, correct
                    ));
                }
            }
            _ => unknown += 1,
        }
    }
    Some(WorkResult {
        score,
        all_correct: feedback.is_empty() && unknown == 0,
        unknown,
        feedback,
    })
}

async fn check_work_result(
    client: &ChaoxingClient,
    course: &Course,
    job: &Job,
    info: &JobInfo,
    submission: &WorkSubmission,
) -> Result<Option<WorkResult>> {
    let base = client.endpoints.mooc1.trim_end_matches('/');
    let work_id = submission.work_id.as_str();
    let mut records = Vec::new();
    for attempt in 0..5 {
        let request = client
            .http
            .client
            .get(format!("{base}/mooc-ans/work/record-list"))
            .query(&[
                ("courseId", course.course_id.as_str()),
                ("classId", course.clazz_id.as_str()),
                ("workId", work_id),
                ("workAnswerId", submission.answer_id.as_str()),
                ("cpi", &course.cpi),
                ("api", "1"),
                ("mooc2", "1"),
                ("ut", "s"),
            ]);
        records = parse_work_records(
            &client
                .fetch_text(request)
                .await
                .context("读取作答记录失败")?,
        );
        if !records.is_empty() {
            break;
        }
        if attempt < 4 {
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
    }
    let Some((times, score)) = records.into_iter().max_by_key(|(times, _)| *times) else {
        let request = client
            .http
            .client
            .get(format!("{base}/mooc-ans/api/work"))
            .query(&[
                ("api", "1"),
                ("workId", work_id),
                ("jobid", job.jobid.as_str()),
                ("originJobId", job.jobid.as_str()),
                ("needRedirect", "true"),
                ("skipHeader", "true"),
                ("knowledgeid", info.knowledgeid.as_str()),
                ("ktoken", info.ktoken.as_str()),
                ("cpi", info.cpi.as_str()),
                ("ut", "s"),
                ("clazzId", course.clazz_id.as_str()),
                ("type", ""),
                ("enc", job.enc.as_str()),
                ("mooc2", "1"),
                ("courseid", course.course_id.as_str()),
            ]);
        let html = client
            .fetch_text(request)
            .await
            .context("重新读取章节检测状态失败")?;
        if html.contains("我的答案") && html.contains("正确答案") {
            let score = PAGE_SCORE
                .captures(&html)
                .and_then(|c| c[1].parse().ok())
                .unwrap_or(0.0);
            return Ok(parse_work_result_detail(&html, score));
        }
        if html.contains("answerwqbid") {
            return Ok(Some(WorkResult {
                score: 0.0,
                all_correct: false,
                unknown: 0,
                feedback: Vec::new(),
            }));
        }
        return Ok(None);
    };
    let request = client
        .http
        .client
        .get(format!("{base}/mooc-ans/work/record-detail"))
        .query(&[
            ("courseId", course.course_id.as_str()),
            ("classId", course.clazz_id.as_str()),
            ("workId", work_id),
            ("workAnswerId", submission.answer_id.as_str()),
            ("times", &times.to_string()),
            ("cpi", course.cpi.as_str()),
            ("ut", "s"),
            ("isdisplaytable", "0"),
            ("firstHeader", "2"),
            ("isWork", "false"),
            ("workSystem", "0"),
            ("api", "1"),
            ("archive", "false"),
            ("mooc2", "1"),
            ("knowledgeid", info.knowledgeid.as_str()),
        ]);
    let body = client
        .fetch_text(request)
        .await
        .context("读取作答详情失败")?;
    Ok(parse_work_result_detail(&body, score))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Endpoints;
    use crate::http::Http;
    use crate::models::{Question, QuestionType};
    use crate::testutil::{MockResponse, MockServer};

    #[test]
    fn save_mode_omits_random_answers_but_keeps_covered_ones() {
        let mut form = WorkForm::default();
        for id in ["1", "2"] {
            let mut question = Question::new("题目", QuestionType::Single, &[]);
            question.id = id.into();
            form.questions.push(question);
        }
        populate_answers(&mut form, &[("A".into(), true), ("B".into(), false)], false);
        assert_eq!(form.get("pyFlag"), Some("1"));
        assert_eq!(form.get("answer1"), Some("A"));
        assert_eq!(form.get("answer2"), Some(""));
        assert_eq!(form.get("answertype2"), Some("0"));
    }

    #[test]
    fn submit_mode_includes_every_answer() {
        let mut form = WorkForm::default();
        let mut question = Question::new("题目", QuestionType::Single, &[]);
        question.id = "1".into();
        form.questions.push(question);
        populate_answers(&mut form, &[("B".into(), false)], true);
        assert_eq!(form.get("pyFlag"), Some(""));
        assert_eq!(form.get("answer1"), Some("B"));
    }

    #[tokio::test]
    async fn prefetch_reads_questions_without_submitting() {
        let html = r#"<form id="form1">
          <input name="courseId" value="1">
          <div class="singleQuesId" data="100"><div class="TiMu" data="0">
            <div class="Zy_TItle"><div>1<span>中国的首都是</span>（2.0分）</div></div>
            <ul><li aria-label="A. 上海选择"></li><li aria-label="B. 北京选择"></li></ul>
          </div></div>
        </form>"#;
        let server = MockServer::start(vec![MockResponse {
            status: 200,
            body: html.into(),
            headers: vec![],
        }])
        .await;
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        let client = Arc::new(ChaoxingClient::new_with_endpoints(
            http,
            Endpoints::all(&server.url),
        ));
        let mut job = Job::new(JobType::Work);
        job.jobid = "work-55".into();
        job.enc = "e1".into();
        prefetch_work(
            client,
            Arc::new(Tiku::disabled()),
            Course {
                course_id: "10".into(),
                clazz_id: "20".into(),
                title: "课程".into(),
                ..Default::default()
            },
            "第一章".into(),
            JobInfo {
                knowledgeid: "1001".into(),
                ktoken: "kt".into(),
                cpi: "30".into(),
                ..Default::default()
            },
            vec![job],
            0.0,
        )
        .await;

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert!(requests[0].path.starts_with("/mooc-ans/api/work?"));
    }

    #[test]
    fn record_feedback_detects_wrong_answer_and_normalizes_judgement() {
        let list = r#"<span class="viewNum">第1次</span><span class="viewScore">80分</span>"#;
        assert_eq!(parse_work_records(list), vec![(1, 80.0)]);
        let detail = r#"<div class="TiMu singleQuesId" data="1">我的答案：</span><div class="fl answerCon">A</div>正确答案：</span><div class="fl answerCon">B</div><div class="TiMu singleQuesId" data="2">我的答案：</span><div class="fl answerCon">对</div>正确答案：</span><div class="fl answerCon">true</div>"#;
        let result = parse_work_result_detail(detail, 80.0).unwrap();
        assert!(!result.all_correct);
        assert_eq!(result.unknown, 0);
        assert_eq!(result.feedback.len(), 1);
    }

    #[test]
    fn result_detail_accepts_separate_wrapper_and_timu_only() {
        let separate = r#"<div class="singleQuesId" data="100"><div class="TiMu" data="0">我的答案：</span><div class="fl answerCon">A</div>正确答案：</span><div class="fl answerCon">B</div></div></div>"#;
        let result = parse_work_result_detail(separate, 80.0).unwrap();
        assert_eq!(result.unknown, 0);
        assert_eq!(result.feedback.len(), 1);

        let timu_only = r#"<div class="TiMu" data="1">我的答案：</span><div class="fl answerCon">对</div>正确答案：</span><div class="fl answerCon">错</div></div>"#;
        let result = parse_work_result_detail(timu_only, 80.0).unwrap();
        assert_eq!(result.unknown, 0);
        assert_eq!(result.feedback.len(), 1);
    }

    #[test]
    fn result_detail_counts_unparseable_blocks() {
        let detail = r#"<div class="TiMu" data="1">没有答案字段</div>"#;
        let result = parse_work_result_detail(detail, 50.0).unwrap();
        assert_eq!(result.unknown, 1);
        assert!(!result.all_correct);
    }
}
