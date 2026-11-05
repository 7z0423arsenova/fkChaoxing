//! 各类章节任务。

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{error::Error, fmt};

use anyhow::{Context, Result, anyhow, bail};
use rand::Rng;
use serde_json::Value;

use crate::client::ChaoxingClient;
use crate::crypto::video_log_enc;
use crate::error::MediaForbidden;
use crate::http::{AUDIO_REFERER, VIDEO_REFERER, snippet, verification_required};
use crate::models::{Course, Job, JobInfo, Point, json_str};

mod count;
mod work;
pub use crate::error::VerificationRequired;
pub use count::increase_learning_count;
pub use work::{prefetch_work, study_work};

/// 视频到达结尾后，服务器仍返回未通过时的最大重报次数。
/// 这里给一个足够长但有上限的次数，避免永久挂起。
const FINAL_REPORT_ATTEMPTS: u32 = if cfg!(test) { 0 } else { 60 };
/// 403 后每次恢复前的等待秒数；测试中不等待。
const MEDIA_RECOVERY_DELAY_SECS: u64 = if cfg!(test) { 0 } else { 2 };
/// 直播打点间隔。学习通播放器每 30 秒调用一次 saveTimePc，
/// 服务器按真实观看时间累计，倍速不会缩短直播任务所需时长。
const LIVE_HEARTBEAT_SECS: u64 = if cfg!(test) { 0 } else { 30 };

#[derive(Debug)]
pub struct MediaNotPassed;

impl fmt::Display for MediaNotPassed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("媒体已经上报到结尾，但服务器未确认完成；请检查网页端任务要求")
    }
}

impl Error for MediaNotPassed {}

/// 直播观看时长未达到服务器要求；继续重试通常没有意义，直接结束该章节。
#[derive(Debug)]
pub struct LiveNotFinished;

impl fmt::Display for LiveNotFinished {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("直播观看时长未达到要求；请稍后重新运行，服务器会从当前进度继续累计")
    }
}

impl Error for LiveNotFinished {}

fn timestamp_ms() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

fn json_i64(value: Option<&Value>) -> Option<i64> {
    value.and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_f64().map(|f| f as i64))
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

fn advance_playback(current: f64, elapsed: Duration, speed: f64, duration: i64) -> f64 {
    (current + elapsed.as_secs_f64() * speed).min(duration as f64)
}

async fn video_status(client: &ChaoxingClient, job: &Job, referer: &str) -> Result<Value> {
    let url = format!(
        "{}/ananas/status/{}",
        client.endpoints.mooc1.trim_end_matches('/'),
        job.objectid
    );
    // waiting 发生在获取 dtoken 之前，与播放倍速无关；给状态接口短暂的恢复时间。
    const WAIT_DELAYS: [u64; 3] = [1, 2, 4];
    for attempt in 0..=WAIT_DELAYS.len() {
        client.wait_request().await;
        let request = client
            .http
            .client
            .get(&url)
            .header(reqwest::header::REFERER, referer)
            .query(&[("k", client.http.fid()), ("flag", "normal".to_owned())]);
        let fetched = client
            .fetch_now(request)
            .await
            .context("读取视频状态失败")?;
        let status = fetched.status;
        let response_url = fetched.url;
        let body = fetched.body;
        if let Some(error) = verification_required(&response_url, &body) {
            tracing::warn!("读取视频状态触发验证：{}", job.display_name());
            return Err(error);
        }
        if status == reqwest::StatusCode::FORBIDDEN {
            tracing::warn!(
                "读取视频状态 403：{}；响应片段：{}",
                job.display_name(),
                snippet(&body, 240)
            );
            return Err(MediaForbidden.into());
        }
        if !status.is_success() {
            bail!("读取视频状态失败：HTTP {status}");
        }
        let data: Value = serde_json::from_str(&body).context("解析视频状态失败")?;
        match data.get("status").and_then(Value::as_str) {
            Some("success") => return Ok(data),
            Some("waiting") if attempt < WAIT_DELAYS.len() => {
                let delay = WAIT_DELAYS[attempt];
                tracing::warn!("媒体状态为 waiting，{delay} 秒后重新查询");
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
            Some("waiting") => bail!("媒体状态持续为 waiting（已查询 4 次）"),
            _ => bail!(
                "媒体状态异常: {}",
                data.get("status").unwrap_or(&Value::Null)
            ),
        }
    }
    unreachable!("媒体状态循环总会返回")
}

async fn video_log(
    client: &ChaoxingClient,
    course: &Course,
    job: &Job,
    dtoken: &str,
    duration: i64,
    playing_time: i64,
    media_type: &str,
    isdrag: &str,
    referer: &str,
) -> Result<bool> {
    if job.otherinfo.contains("courseId") {
        bail!("视频任务 otherinfo 包含无效的 courseId");
    }
    let uid = client.http.uid()?;
    let enc = video_log_enc(
        &course.clazz_id,
        &uid,
        &job.jobid,
        &job.objectid,
        playing_time,
        duration,
    );
    let url = format!(
        "{}/mooc-ans/multimedia/log/a/{}/{}",
        client.endpoints.mooc1.trim_end_matches('/'),
        course.cpi,
        dtoken
    );
    let rt = if !job.rt.is_empty() {
        job.rt.as_str()
    } else if job.otherinfo.contains("-rt_d") {
        "0.9"
    } else {
        "1"
    };
    let playing = playing_time.to_string();
    let duration_text = duration.to_string();
    let clip = format!("0_{duration}");
    let mut params = vec![
        ("clazzId", course.clazz_id.as_str()),
        ("playingTime", playing.as_str()),
        ("duration", duration_text.as_str()),
        ("clipTime", clip.as_str()),
        ("objectId", job.objectid.as_str()),
        ("otherInfo", job.otherinfo.as_str()),
        ("courseId", course.course_id.as_str()),
        ("jobid", job.jobid.as_str()),
        ("userid", uid.as_str()),
        ("isdrag", isdrag),
        ("view", "pc"),
        ("enc", enc.as_str()),
        ("dtype", media_type),
        ("rt", rt),
    ];
    if !job.video_face_capture_enc.is_empty() {
        params.push(("videoFaceCaptureEnc", &job.video_face_capture_enc));
    }
    if !job.att_duration.is_empty() {
        params.push(("attDuration", &job.att_duration));
    }
    if !job.att_duration_enc.is_empty() {
        params.push(("attDurationEnc", &job.att_duration_enc));
    }
    let now = timestamp_ms();
    params.push(("_t", &now));
    client.wait_video().await;
    let request = client
        .http
        .client
        .get(&url)
        .header(reqwest::header::REFERER, referer)
        .query(&params);
    let fetched = client
        .fetch_now(request)
        .await
        .context("上报媒体进度失败")?;
    let status = fetched.status;
    let response_url = fetched.url;
    let body = fetched.body;
    if let Some(error) = verification_required(&response_url, &body) {
        tracing::warn!(
            "媒体进度上报触发验证：{} / {} / {media_type}",
            course.title,
            job.display_name()
        );
        return Err(error);
    }
    if body.contains("验证码") {
        tracing::warn!(
            "媒体进度上报触发验证：{} / {} / {media_type}；{}；响应片段：{}",
            course.title,
            job.display_name(),
            response_url,
            snippet(&body, 240)
        );
        return Err(VerificationRequired.into());
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        tracing::warn!(
            "媒体进度上报 403：{} / {} / {media_type}；{}；响应片段：{}",
            course.title,
            job.display_name(),
            response_url,
            snippet(&body, 240)
        );
        return Err(MediaForbidden.into());
    }
    if !status.is_success() {
        bail!("媒体进度上报失败：HTTP {status}");
    }
    let data: Value = serde_json::from_str(&body).context("解析媒体进度响应失败")?;
    Ok(data
        .get("isPassed")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

pub async fn study_video(
    client: &ChaoxingClient,
    course: &Course,
    job: &Job,
    speed: f64,
    instant_complete: bool,
) -> Result<()> {
    let mut progress = crate::console::TaskProgress::new(
        format!(
            "{} / 班级 {} / {}",
            course.title,
            course.clazz_id,
            job.display_name()
        ),
        0,
    );
    let mut last_error = None;
    let playback = PlaybackOptions {
        speed,
        instant_complete,
    };
    // 卡片明确是音频任务时先按 Audio 上报；普通视频任务仍先试 Video，
    // 失败后再回退到 Audio（兼容把音频误标成 video 的课程）。
    let media_types = if job.is_audio {
        [("Audio", AUDIO_REFERER), ("Video", VIDEO_REFERER)]
    } else {
        [("Video", VIDEO_REFERER), ("Audio", AUDIO_REFERER)]
    };
    for (media_type, referer) in media_types {
        match study_media(
            client, course, job, playback, media_type, referer, &progress,
        )
        .await
        {
            Ok(()) => {
                progress.finish(true);
                return Ok(());
            }
            Err(error)
                if error.downcast_ref::<VerificationRequired>().is_some()
                    || error.downcast_ref::<MediaNotPassed>().is_some() =>
            {
                return Err(error);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("媒体任务失败")))
}

#[derive(Debug, Clone, Copy)]
struct PlaybackOptions {
    speed: f64,
    instant_complete: bool,
}

/// 一次媒体播放的状态。403 时负责刷新状态、重登和切换 rt。
struct MediaSession<'a> {
    client: &'a ChaoxingClient,
    course: &'a Course,
    job: Job,
    media_type: &'a str,
    referer: &'a str,
    duration: i64,
    dtoken: String,
    play_time: i64,
    relogged: bool,
}

impl<'a> MediaSession<'a> {
    async fn open(
        client: &'a ChaoxingClient,
        course: &'a Course,
        job: &Job,
        media_type: &'a str,
        referer: &'a str,
    ) -> Result<Self> {
        let mut relogged = false;
        let mut attempts: u8 = 0;
        let status = loop {
            match video_status(client, job, referer).await {
                Ok(status) => break status,
                Err(error) if error.downcast_ref::<MediaForbidden>().is_some() && attempts < 2 => {
                    attempts += 1;
                    tracing::warn!(
                        "读取媒体状态 403（{attempts}/2）：{} / {}",
                        course.title,
                        job.display_name()
                    );
                    tokio::time::sleep(Duration::from_secs(
                        MEDIA_RECOVERY_DELAY_SECS + u64::from(attempts) * 2,
                    ))
                    .await;
                    if !relogged {
                        relogged = true;
                        match client.relogin().await {
                            Ok(true) => tracing::info!("媒体状态 403，已重新登录"),
                            Ok(false) => {
                                tracing::warn!("媒体状态 403，没有可用于重登的账号密码")
                            }
                            Err(error) => {
                                tracing::warn!("媒体状态 403，重新登录失败：{error:#}")
                            }
                        }
                    }
                }
                Err(error) => return Err(error),
            }
        };
        let duration = json_i64(status.get("duration"))
            .filter(|v| *v >= 0)
            .ok_or_else(|| anyhow!("媒体状态缺少 duration"))?;
        let dtoken = status
            .get("dtoken")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("媒体状态缺少 dtoken"))?
            .to_owned();
        let play_time = (job.play_time_ms / 1000).clamp(0, duration);
        Ok(Self {
            client,
            course,
            job: job.clone(),
            media_type,
            referer,
            duration,
            dtoken,
            play_time,
            relogged,
        })
    }

    async fn refresh(&mut self) -> Result<()> {
        let status = video_status(self.client, &self.job, self.referer).await?;
        self.duration = json_i64(status.get("duration"))
            .filter(|v| *v >= 0)
            .ok_or_else(|| anyhow!("刷新媒体状态缺少 duration"))?;
        let dtoken = status
            .get("dtoken")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("刷新媒体状态缺少 dtoken"))?
            .to_owned();
        self.dtoken = dtoken;
        if let Some(play_time) = json_i64(status.get("playTime")) {
            self.play_time = play_time.clamp(0, self.duration);
        }
        Ok(())
    }

    fn rotate_rt(&mut self) {
        let next = match self.job.rt.as_str() {
            "0.9" => "1",
            "1" => "0.9",
            _ => "0.9",
        };
        tracing::info!("切换 rt={next} 后重试媒体进度上报");
        self.job.rt = next.to_owned();
    }

    async fn recover(&mut self, attempt: u8) -> Result<()> {
        tokio::time::sleep(Duration::from_secs(
            MEDIA_RECOVERY_DELAY_SECS + u64::from(attempt) * 2,
        ))
        .await;
        if let Err(error) = self.refresh().await {
            if error.downcast_ref::<VerificationRequired>().is_some() {
                return Err(error);
            }
            tracing::warn!("刷新媒体状态失败，继续重试：{error:#}");
        }
        if !self.relogged {
            self.relogged = true;
            match self.client.relogin().await {
                Ok(true) => {
                    tracing::info!("媒体上报 403，已重新登录");
                    if let Err(error) = self.refresh().await {
                        if error.downcast_ref::<VerificationRequired>().is_some() {
                            return Err(error);
                        }
                        tracing::warn!("重新登录后刷新媒体状态失败：{error:#}");
                    }
                }
                Ok(false) => {
                    tracing::warn!("媒体上报 403，没有可用于重登的账号密码，改为切换 rt");
                    self.rotate_rt();
                }
                Err(error) => {
                    tracing::warn!("媒体上报 403，重新登录失败，改为切换 rt：{error:#}");
                    self.rotate_rt();
                }
            }
        } else {
            self.rotate_rt();
        }
        Ok(())
    }

    /// 上报一次进度；遇到 403 最多恢复 3 次。
    async fn log(&mut self, mut playing_time: i64, isdrag: &str) -> Result<bool> {
        let mut last_error = None;
        for attempt in 0..3 {
            // 拖到结尾的报文始终使用当前 duration；普通上报在恢复后使用服务器
            // 返回的 playTime，避免拿恢复前的本地时间覆盖服务器进度。
            let time = if isdrag == "4" {
                self.duration
            } else {
                playing_time.clamp(0, self.duration)
            };
            match video_log(
                self.client,
                self.course,
                &self.job,
                &self.dtoken,
                self.duration,
                time,
                self.media_type,
                isdrag,
                self.referer,
            )
            .await
            {
                Ok(passed) => return Ok(passed),
                Err(error) if error.downcast_ref::<MediaForbidden>().is_some() => {
                    tracing::warn!(
                        "媒体进度上报 403（{}/3）：{} / {} / {}",
                        attempt + 1,
                        self.course.title,
                        self.job.display_name(),
                        self.media_type
                    );
                    last_error = Some(error);
                    self.recover(attempt as u8).await?;
                    playing_time = self.play_time;
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("媒体进度上报失败")))
    }
}

async fn study_media(
    client: &ChaoxingClient,
    course: &Course,
    job: &Job,
    playback: PlaybackOptions,
    media_type: &str,
    referer: &str,
    progress: &crate::console::TaskProgress,
) -> Result<()> {
    let mut session = MediaSession::open(client, course, job, media_type, referer).await?;
    progress.update(session.play_time as u64, session.duration as u64);
    // 与 Python 版一致：先尝试按“拖到结尾”上报一次（isdrag=4）。
    // 服务器若直接确认完成，就不用再按真实播放时间等待；失败则继续正常上报。
    if playback.instant_complete
        && session.duration > 0
        && session.log(session.duration, "4").await?
    {
        return Ok(());
    }
    if session.log(session.play_time, "3").await? {
        return Ok(());
    }
    let mut elapsed_playback = session.play_time as f64;
    let mut final_attempts = 0;
    let mut last_log_time = 0;
    let mut next_log = rand::rng().random_range(30..=90);
    let speed = playback.speed.clamp(1.0, 2.0);
    let mut previous = Instant::now();
    loop {
        if session.play_time - last_log_time >= next_log || session.play_time >= session.duration {
            if session.play_time >= session.duration {
                final_attempts += 1;
                if final_attempts > FINAL_REPORT_ATTEMPTS {
                    return Err(MediaNotPassed.into());
                }
            }
            if session.log(session.play_time, "3").await? {
                return Ok(());
            }
            // 403 恢复可能把 play_time 改成服务器返回的进度，必须同步本地时钟，
            // 否则下一轮会用恢复前的旧时间覆盖它。
            elapsed_playback = session.play_time as f64;
            progress.update(session.play_time as u64, session.duration as u64);
            next_log = rand::rng().random_range(30..=90);
            last_log_time = session.play_time;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let now = Instant::now();
        elapsed_playback =
            advance_playback(elapsed_playback, now - previous, speed, session.duration);
        session.play_time = elapsed_playback as i64;
        progress.update(session.play_time as u64, session.duration as u64);
        previous = now;
    }
}

pub async fn study_document(client: &ChaoxingClient, course: &Course, job: &Job) -> Result<()> {
    let knowledgeid = job
        .otherinfo
        .split("nodeId_")
        .nth(1)
        .and_then(|tail| tail.split('-').next())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("文档任务缺少 knowledgeid"))?;
    let url = format!(
        "{}/ananas/job/document",
        client.endpoints.mooc1.trim_end_matches('/')
    );
    client.wait_request().await;
    let request = client.http.client.get(url).query(&[
        ("jobid", job.jobid.as_str()),
        ("knowledgeid", knowledgeid),
        ("courseid", course.course_id.as_str()),
        ("clazzid", course.clazz_id.as_str()),
        ("jtoken", job.jtoken.as_str()),
    ]);
    client
        .fetch_text_now(request)
        .await
        .context("完成文档任务失败")?;
    Ok(())
}

pub async fn study_read(
    client: &ChaoxingClient,
    course: &Course,
    job: &Job,
    info: &JobInfo,
) -> Result<()> {
    let url = format!(
        "{}/ananas/job/readv2",
        client.endpoints.mooc1.trim_end_matches('/')
    );
    client.wait_request().await;
    let request = client.http.client.get(url).query(&[
        ("jobid", job.jobid.as_str()),
        ("knowledgeid", info.knowledgeid.as_str()),
        ("jtoken", job.jtoken.as_str()),
        ("courseid", course.course_id.as_str()),
        ("clazzid", course.clazz_id.as_str()),
    ]);
    client
        .fetch_text_now(request)
        .await
        .context("完成阅读任务失败")?;
    Ok(())
}

pub async fn study_empty_page(
    client: &ChaoxingClient,
    course: &Course,
    point: &Point,
) -> Result<()> {
    let url = format!(
        "{}/mooc-ans/mycourse/studentstudyAjax",
        client.endpoints.mooc1.trim_end_matches('/')
    );
    client.wait_request().await;
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
    client
        .fetch_text_now(request)
        .await
        .context("访问空任务章节失败")?;
    Ok(())
}

async fn fetch_live_status(
    client: &ChaoxingClient,
    course: &Course,
    job: &Job,
    info: &JobInfo,
    live_id: &str,
    uid: &str,
) -> Result<Value> {
    client.wait_request().await;
    let request = client
        .http
        .client
        .get(format!(
            "{}/ananas/live/liveinfo",
            client.endpoints.mooc1.trim_end_matches('/')
        ))
        .query(&[
            ("liveid", live_id),
            ("userid", uid),
            ("clazzid", course.clazz_id.as_str()),
            ("knowledgeid", info.knowledgeid.as_str()),
            ("courseid", course.course_id.as_str()),
            ("jobid", job.jobid.as_str()),
            ("ut", "s"),
        ]);
    let body = client
        .fetch_text_now(request)
        .await
        .context("读取直播状态失败")?;
    serde_json::from_str(&body).context("解析直播状态失败")
}

pub async fn study_live(
    client: &ChaoxingClient,
    course: &Course,
    job: &Job,
    info: &JobInfo,
    _speed: f64,
) -> Result<()> {
    let prop = &job.property;
    // liveId 在学习通返回的 JSON 中是数字，streamName/vdoid 是字符串；
    // 这里统一转成字符串，避免把数字类型的 liveId 当成缺失。
    let required = |key: &str| {
        prop.get(key)
            .map(json_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow!("直播任务缺少 {key}"))
    };
    let live_id = required("liveId")?;
    let stream_name = required("streamName")?;
    let vdoid = required("vdoid")?;
    let fid = prop.get("fid").map(json_str).unwrap_or_default();
    let uid = client.http.uid()?;

    // 学习通播放器会先调用这个接口把直播和课程章节关联起来；
    // 缺少这一步时 saveTimePc 虽然返回 @success，但章节观看时长不会更新。
    if !fid.is_empty() {
        client.wait_request().await;
        let request = client
            .http
            .client
            .get(format!(
                "{}/live/savecourselive",
                client.endpoints.zhibo.trim_end_matches('/')
            ))
            .query(&[
                ("streamName", stream_name.as_str()),
                ("vdoid", vdoid.as_str()),
                ("courseId", course.course_id.as_str()),
                ("fid", fid.as_str()),
            ]);
        let body = client
            .fetch_text_now(request)
            .await
            .context("关联直播和课程失败")?;
        tracing::debug!("关联直播和课程: {body}");
    }

    let mut status = fetch_live_status(client, course, job, info, &live_id, &uid).await?;
    let duration = json_i64(status.pointer("/temp/data/duration"))
        .unwrap_or(1800)
        .max(0) as u64;
    let target_ms = duration as i64 * 1000;
    let mut watched_ms = json_i64(status.pointer("/temp/data/timeLong")).unwrap_or(0);
    if watched_ms >= target_ms {
        return Ok(());
    }

    let interval = Duration::from_secs(LIVE_HEARTBEAT_SECS);
    let max_rounds = (duration / LIVE_HEARTBEAT_SECS.max(1) + 120).max(1);
    for round in 0..max_rounds {
        client.wait_request().await;
        let request = client
            .http
            .client
            .get(format!(
                "{}/saveTimePc",
                client.endpoints.zhibo.trim_end_matches('/')
            ))
            .query(&[
                ("streamName", stream_name.as_str()),
                ("vdoid", vdoid.as_str()),
                ("userId", &uid),
                ("isStart", "0"),
                ("t", &timestamp_ms()),
                ("courseId", course.course_id.as_str()),
            ]);
        if client
            .fetch_text_now(request)
            .await
            .context("提交直播观看时长失败")?
            .trim()
            != "@success"
        {
            bail!("直播观看时长未被服务器接受");
        }
        tokio::time::sleep(interval).await;
        status = fetch_live_status(client, course, job, info, &live_id, &uid).await?;
        watched_ms = json_i64(status.pointer("/temp/data/timeLong")).unwrap_or(watched_ms);
        tracing::info!(
            "直播进度：{:.1}%（{} / {} 秒）",
            watched_ms as f64 / target_ms.max(1) as f64 * 100.0,
            watched_ms / 1000,
            duration
        );
        if watched_ms >= target_ms {
            return Ok(());
        }
        if round + 1 == max_rounds {
            break;
        }
    }
    tracing::warn!(
        "直播观看时长未达到要求：{:.1}%（{} / {} 秒）",
        watched_ms as f64 / target_ms.max(1) as f64 * 100.0,
        watched_ms / 1000,
        duration
    );
    Err(LiveNotFinished.into())
}

#[cfg(test)]
mod tests {
    use super::{
        MediaSession, study_document, study_empty_page, study_live, study_read, study_video,
        video_log, video_status,
    };

    #[test]
    fn media_progress_keeps_fractional_seconds_between_ticks() {
        let first = super::advance_playback(0.0, std::time::Duration::from_millis(500), 1.0, 60);
        let second = super::advance_playback(first, std::time::Duration::from_millis(500), 1.0, 60);
        assert_eq!(second as i64, 1);
    }
    use std::sync::Arc;

    use crate::client::{ChaoxingClient, Endpoints};
    use crate::http::Http;
    use crate::models::{Course, Job, JobInfo, JobType, Point};
    use crate::testutil::{MockResponse, MockServer};

    #[tokio::test]
    async fn waiting_media_status_is_polled_until_ready() {
        let server = MockServer::start(vec![
            MockResponse::json(200, serde_json::json!({"status":"waiting"})),
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token","duration":60}),
            ),
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let mut job = Job::new(JobType::Video);
        job.objectid = "obj".into();

        let status = video_status(&client, &job, crate::http::VIDEO_REFERER)
            .await
            .unwrap();
        assert_eq!(status["dtoken"], "token");
        assert_eq!(server.requests().len(), 2);
    }

    #[tokio::test]
    async fn persistent_waiting_media_status_returns_an_error() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                serde_json::json!({"status":"waiting"})
            );
            4
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let mut job = Job::new(JobType::Video);
        job.objectid = "obj".into();

        let error = video_status(&client, &job, crate::http::VIDEO_REFERER)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("waiting"));
        assert_eq!(server.requests().len(), 4);
    }

    #[tokio::test]
    async fn document_and_read_tasks_send_the_required_ids() {
        let server = MockServer::start(vec![
            MockResponse {
                status: 200,
                body: String::new(),
                headers: vec![],
            },
            MockResponse::json(200, serde_json::json!({"msg": "ok"})),
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            ..Default::default()
        };
        let mut document = Job::new(JobType::Document);
        document.jobid = "d1".into();
        document.otherinfo = "nodeId_30-cpi_5".into();
        document.jtoken = "doc-token".into();
        let mut read = Job::new(JobType::Read);
        read.jobid = "r1".into();
        read.jtoken = "read-token".into();
        let info = JobInfo {
            knowledgeid: "30".into(),
            ..Default::default()
        };

        study_document(&client, &course, &document).await.unwrap();
        study_read(&client, &course, &read, &info).await.unwrap();

        let requests = server.requests();
        assert!(requests[0].path.starts_with("/ananas/job/document?"));
        assert!(requests[0].path.contains("knowledgeid=30"));
        assert!(requests[1].path.starts_with("/ananas/job/readv2?"));
        assert!(requests[1].path.contains("jtoken=read-token"));
    }

    #[tokio::test]
    async fn document_and_empty_page_reject_http_errors() {
        let server = MockServer::start(vec![
            MockResponse {
                status: 500,
                body: "server error".into(),
                headers: vec![],
            },
            MockResponse {
                status: 500,
                body: "server error".into(),
                headers: vec![],
            },
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut document = Job::new(JobType::Document);
        document.otherinfo = "nodeId_30-cpi_1-rt_d".into();
        document.jtoken = "doc-token".into();
        let point = Point {
            id: "9".into(),
            ..Default::default()
        };

        assert!(study_document(&client, &course, &document).await.is_err());
        assert!(study_empty_page(&client, &course, &point).await.is_err());
    }

    #[tokio::test]
    async fn video_reports_current_progress_and_stops_when_passed() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token","duration":60}),
            )
            .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse::json(200, serde_json::json!({"isPassed":true})),
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Video);
        job.jobid = "job".into();
        job.objectid = "obj".into();

        study_video(&client, &course, &job, 1.0, false)
            .await
            .unwrap();

        let requests = server.requests();
        assert!(requests[0].path.starts_with("/ananas/status/obj?"));
        assert!(
            requests[1]
                .path
                .starts_with("/mooc-ans/multimedia/log/a/30/token?")
        );
        assert!(requests[1].path.contains("playingTime=0"));
        assert!(requests[1].path.contains("isdrag=3"));
    }

    #[tokio::test]
    async fn video_initial_report_uses_recorded_play_time() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token","duration":60}),
            )
            .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse::json(200, serde_json::json!({"isPassed":true})),
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Video);
        job.jobid = "job".into();
        job.objectid = "obj".into();
        job.play_time_ms = 12_000;

        study_video(&client, &course, &job, 1.0, false)
            .await
            .unwrap();
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].path.contains("playingTime=12"));
        assert!(!requests[1].path.contains("playingTime=60"));
    }

    #[tokio::test]
    async fn instant_complete_reports_full_duration_with_drag_flag() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token","duration":60}),
            )
            .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse::json(200, serde_json::json!({"isPassed":true})),
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Video);
        job.jobid = "job".into();
        job.objectid = "obj".into();
        job.play_time_ms = 12_000;

        study_video(&client, &course, &job, 1.0, true)
            .await
            .unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].path.contains("playingTime=60"));
        assert!(requests[1].path.contains("isdrag=4"));
        assert_eq!(
            requests[1].header("referer"),
            Some(crate::http::VIDEO_REFERER)
        );
    }

    #[tokio::test]
    async fn instant_complete_falls_back_to_normal_report() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token","duration":60}),
            )
            .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse::json(200, serde_json::json!({"isPassed":false})),
            MockResponse::json(200, serde_json::json!({"isPassed":true})),
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Video);
        job.jobid = "job".into();
        job.objectid = "obj".into();
        job.play_time_ms = 12_000;

        study_video(&client, &course, &job, 1.0, true)
            .await
            .unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests[1].path.contains("playingTime=60"));
        assert!(requests[1].path.contains("isdrag=4"));
        assert!(requests[2].path.contains("playingTime=12"));
        assert!(requests[2].path.contains("isdrag=3"));
    }

    #[tokio::test]
    async fn media_status_403_retries_after_recovery() {
        let server = MockServer::start(vec![
            MockResponse::json(403, serde_json::json!({"msg":"forbidden"})),
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token","duration":60}),
            )
            .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse::json(200, serde_json::json!({"isPassed":true})),
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Video);
        job.jobid = "job".into();
        job.objectid = "obj".into();

        study_video(&client, &course, &job, 1.0, true)
            .await
            .unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].path.starts_with("/ananas/status/obj?"));
        assert!(requests[1].path.starts_with("/ananas/status/obj?"));
        assert!(requests[2].path.contains("isdrag=4"));
    }

    #[tokio::test]
    async fn media_duration_accepts_float() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token","duration":298.5}),
            )
            .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse::json(200, serde_json::json!({"isPassed":true})),
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Video);
        job.jobid = "job".into();
        job.objectid = "obj".into();

        study_video(&client, &course, &job, 1.0, true)
            .await
            .unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].path.contains("duration=298"));
        assert!(requests[1].path.contains("playingTime=298"));
    }

    #[tokio::test]
    async fn media_recovery_uses_server_play_time() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token","duration":600}),
            )
            .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse::json(403, serde_json::json!({"msg":"forbidden"})),
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token2","duration":600,"playTime":300}),
            ),
            MockResponse::json(200, serde_json::json!({"isPassed":true})),
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Video);
        job.jobid = "job".into();
        job.objectid = "obj".into();

        let mut session =
            MediaSession::open(&client, &course, &job, "Video", crate::http::VIDEO_REFERER)
                .await
                .unwrap();
        assert!(session.log(100, "3").await.unwrap());

        let requests = server.requests();
        assert_eq!(requests.len(), 4);
        assert!(requests[1].path.contains("playingTime=100"));
        assert!(requests[3].path.contains("playingTime=300"));
    }

    #[tokio::test]
    async fn video_log_does_not_treat_invalidate_as_verification() {
        let cookie_path =
            std::env::temp_dir().join(format!("chaoxing-video-log-{}.txt", std::process::id()));
        std::fs::write(&cookie_path, "_uid=42; fid=1024").unwrap();
        let server = MockServer::start(vec![MockResponse::json(
            200,
            serde_json::json!({"isPassed": false, "msg": "invalidate"}),
        )])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new(&cookie_path).unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Video);
        job.jobid = "job".into();
        job.objectid = "obj".into();

        let passed = video_log(
            &client,
            &course,
            &job,
            "token",
            60,
            0,
            "Video",
            "3",
            crate::http::VIDEO_REFERER,
        )
        .await
        .unwrap();

        assert!(!passed);
        let _ = std::fs::remove_file(cookie_path);
    }

    #[tokio::test]
    async fn media_403_refreshes_status_and_switches_rt() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token","duration":60}),
            )
            .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse::json(403, serde_json::json!({"msg":"forbidden"})),
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token2","duration":60}),
            ),
            MockResponse::json(200, serde_json::json!({"isPassed":true})),
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Video);
        job.jobid = "job".into();
        job.objectid = "obj".into();

        study_video(&client, &course, &job, 1.0, true)
            .await
            .unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 4);
        assert!(requests[1].path.contains("isdrag=4"));
        assert!(requests[2].path.starts_with("/ananas/status/obj?"));
        assert!(requests[3].path.contains("isdrag=4"));
        assert!(requests[3].path.contains("rt=0.9"));
    }

    #[tokio::test]
    async fn audio_task_reports_with_audio_dtype_first() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token","duration":60}),
            )
            .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse::json(200, serde_json::json!({"isPassed":true})),
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Video);
        job.is_audio = true;
        job.jobid = "job".into();
        job.objectid = "obj".into();

        study_video(&client, &course, &job, 1.0, false)
            .await
            .unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].path.contains("dtype=Audio"));
        assert_eq!(
            requests[1].header("referer"),
            Some(crate::http::AUDIO_REFERER)
        );
    }

    #[tokio::test]
    async fn verification_block_does_not_request_obsolete_captcha_or_try_audio() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token","duration":60}),
            )
            .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse {
                status: 403,
                body: "<html>【9010】操作异常，请输入图片中的验证码</html>".into(),
                headers: vec![],
            },
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Video);
        job.jobid = "job".into();
        job.objectid = "obj".into();

        let error = study_video(&client, &course, &job, 1.0, false)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("安全验证"), "{error:#}");
        assert_eq!(server.requests().len(), 2);
    }

    #[tokio::test]
    async fn unconfirmed_end_of_video_does_not_try_audio_or_repeat_report() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                serde_json::json!({"status":"success","dtoken":"token","duration":60}),
            )
            .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse::json(200, serde_json::json!({"isPassed":false})),
        ])
        .await;
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new("/nonexistent/cookies.txt").unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            cpi: "30".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Video);
        job.jobid = "job".into();
        job.objectid = "obj".into();
        job.play_time_ms = 60_000;

        let error = study_video(&client, &course, &job, 1.0, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("未确认完成"), "{error:#}");
        assert_eq!(server.requests().len(), 2);
    }

    #[tokio::test]
    async fn live_task_accepts_numeric_live_id() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                serde_json::json!({"temp":{"data":{"duration":60,"timeLong":0}}}),
            ),
            MockResponse {
                status: 200,
                body: "@success".into(),
                headers: vec![],
            },
            MockResponse::json(
                200,
                serde_json::json!({"temp":{"data":{"duration":60,"timeLong":60000}}}),
            ),
        ])
        .await;
        let cookie_path =
            std::env::temp_dir().join(format!("chaoxing-live-cookie-{}.txt", std::process::id()));
        std::fs::write(&cookie_path, "_uid=42").unwrap();
        let client = ChaoxingClient::new_with_endpoints(
            Arc::new(Http::new(&cookie_path).unwrap()),
            Endpoints::all(&server.url),
        );
        let course = Course {
            course_id: "10".into(),
            clazz_id: "20".into(),
            ..Default::default()
        };
        let mut job = Job::new(JobType::Live);
        job.jobid = "live-1".into();
        job.property = serde_json::json!({
            "liveId": 8000292482101802u64,
            "streamName": "stream",
            "vdoid": "vdoid"
        });
        let info = JobInfo {
            knowledgeid: "30".into(),
            ..Default::default()
        };

        study_live(&client, &course, &job, &info, 2.0)
            .await
            .unwrap();

        let requests = server.requests();
        assert!(requests[0].path.starts_with("/ananas/live/liveinfo?"));
        assert!(requests[0].path.contains("liveid=8000292482101802"));
        assert!(requests[1].path.starts_with("/saveTimePc?"));
        assert!(requests[1].path.contains("streamName=stream"));
        let _ = std::fs::remove_file(cookie_path);
    }
}
