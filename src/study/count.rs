//! 章节访问计次：访问页面、执行页面给出的 setlog、发送监控心跳。

use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use regex::Regex;
use tracing::warn;

use crate::client::ChaoxingClient;
use crate::models::{Course, Point};

use super::VerificationRequired;

static SETLOG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<script[^>]+src="(https://fystat-ans\.chaoxing\.com/log/setlog[^"]+)""#).unwrap()
});

pub async fn increase_learning_count(
    client: &ChaoxingClient,
    course: &Course,
    points: &[Point],
    target: u32,
) -> Result<()> {
    if points.is_empty() {
        return Ok(());
    }
    let mut total = 0;
    let mut progress = crate::console::TaskProgress::new(
        format!("{} / 班级 {} / 章节学习次数", course.title, course.clazz_id),
        target as u64,
    );
    let mut consecutive_failures = 0;
    while total < target {
        for point in points {
            if total >= target {
                break;
            }
            match access_and_count(client, course, point).await {
                Ok(()) => {
                    consecutive_failures = 0;
                    total += 1;
                    progress.update(total as u64, target as u64);
                }
                Err(error) => {
                    // 计次遇到验证码时重试没有意义，直接让整轮任务停下来。
                    if error.downcast_ref::<VerificationRequired>().is_some() {
                        return Err(error);
                    }
                    consecutive_failures += 1;
                    warn!("章节计次失败：{}: {error:#}", point.title);
                    if consecutive_failures >= 10 {
                        bail!("章节计次连续失败 10 次");
                    }
                }
            }
        }
    }
    progress.finish(true);
    Ok(())
}

async fn access_and_count(client: &ChaoxingClient, course: &Course, point: &Point) -> Result<()> {
    let url = format!(
        "{}/mooc-ans/mycourse/studentstudyAjax",
        client.endpoints.mooc1.trim_end_matches('/')
    );
    let request = client.http.client.get(url).query(&[
        ("courseId", course.course_id.as_str()),
        ("clazzid", course.clazz_id.as_str()),
        ("chapterId", point.id.as_str()),
        ("cpi", course.cpi.as_str()),
        ("verificationcode", ""),
        ("mooc2", "1"),
        ("microTopicId", "0"),
        ("editorPreview", "0"),
    ]);
    let html = client
        .fetch_text(request)
        .await
        .context("读取章节计次页面失败")?;
    if let Some(url) = SETLOG
        .captures(&html)
        .map(|capture| capture[1].replace("&amp;", "&"))
    {
        client.wait_request().await;
        let _ = client.http.client.get(url).send().await;
    }
    heartbeat(client, course, point).await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    heartbeat(client, course, point).await;
    Ok(())
}

async fn heartbeat(client: &ChaoxingClient, course: &Course, point: &Point) {
    client.wait_request().await;
    let referer = format!(
        "{}/mycourse/studentstudy?chapterId={}&courseId={}&clazzid={}&cpi={}&mooc2=1",
        client.endpoints.mooc1.trim_end_matches('/'),
        point.id,
        course.course_id,
        course.clazz_id,
        course.cpi
    );
    let _ = client
        .http
        .client
        .get("https://detect.chaoxing.com/api/monitor")
        .header(reqwest::header::REFERER, referer)
        .query(&[
            ("version", super::timestamp_ms()),
            ("refer", "http://i.mooc.chaoxing.com".into()),
            ("from", "".into()),
            ("fid", client.http.fid()),
            ("jsoncallback", format!("jsonp{}", super::timestamp_ms())),
            ("t", super::timestamp_ms()),
        ])
        .timeout(Duration::from_secs(5))
        .send()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_chaoxing_setlog_url() {
        let html = r#"<script src="https://fystat-ans.chaoxing.com/log/setlog?encode=abc&amp;t=1"></script>"#;
        assert_eq!(
            SETLOG.captures(html).map(|c| c[1].to_owned()),
            Some("https://fystat-ans.chaoxing.com/log/setlog?encode=abc&amp;t=1".to_owned())
        );
    }
}
