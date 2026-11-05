//! 学习通媒体进度抓包重放。
//!
//! 播放器在播放音频/视频时，会周期性请求
//! `/mooc-ans/multimedia/log/a/{cpi}/{dtoken}`，报文里带 `playingTime`、
//! `duration`、`enc` 等参数。把这些请求从 HAR 抓包里按时间顺序取出来，
//! 依次发给真实服务器，就能复现一次真实播放产生的进度。
//!
//! 重放只依赖抓到的报文，不依赖媒体文件本身：本地 MP3、对象存储里的云端
//! MP3，甚至视频，只要抓包时真实播放过一次即可。

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use chrono::DateTime;
use serde_json::Value;
use url::Url;

use crate::crypto::video_log_enc;
use crate::http::{Http, verification_required};

/// 学习通媒体进度接口路径中的固定片段。
const MEDIA_LOG_MARKER: &str = "/multimedia/log/";

/// 只重放某一种媒体类型的报文。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MediaKind {
    #[default]
    All,
    Audio,
    Video,
}

impl MediaKind {
    fn matches(self, dtype: Option<&str>) -> bool {
        match self {
            Self::All => true,
            Self::Audio => dtype.is_some_and(|value| value.eq_ignore_ascii_case("audio")),
            Self::Video => dtype.is_some_and(|value| value.eq_ignore_ascii_case("video")),
        }
    }
}

/// 从抓包里提取出来的一条请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRequest {
    /// 在抓包文件中的原始顺序，用于时间相同时稳定排序。
    pub order: usize,
    /// HAR 的 `startedDateTime`，其他格式可以为空。
    pub started_at: Option<String>,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

impl CapturedRequest {
    /// URL 查询参数的原始顺序。
    pub fn query_pairs(&self) -> Vec<(String, String)> {
        Url::parse(&self.url)
            .map(|url| {
                url.query_pairs()
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn query(&self, name: &str) -> Option<String> {
        self.query_pairs()
            .into_iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn dtype(&self) -> Option<String> {
        self.query("dtype")
    }

    pub fn playing_time(&self) -> Option<i64> {
        self.query("playingTime")
            .and_then(|value| value.parse().ok())
    }

    pub fn duration(&self) -> Option<i64> {
        self.query("duration").and_then(|value| value.parse().ok())
    }

    /// 是否属于学习通的媒体进度上报接口。
    pub fn is_media_log(&self) -> bool {
        Url::parse(&self.url)
            .map(|url| url.path().contains(MEDIA_LOG_MARKER))
            .unwrap_or(false)
    }

    /// 形如 `00:30 / 12:34` 的进度描述，供计划预览使用。
    pub fn progress_label(&self) -> String {
        format!(
            "{} / {}",
            format_seconds(self.playing_time()),
            format_seconds(self.duration())
        )
    }
}

fn format_seconds(value: Option<i64>) -> String {
    let Some(value) = value else {
        return "?".to_owned();
    };
    let value = value.max(0);
    let (hours, minutes, seconds) = (value / 3600, value % 3600 / 60, value % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// 读取抓包文件。支持 Chrome/Edge 导出的 HAR、请求对象数组，以及 JSONL。
pub fn load_capture(path: &Path) -> Result<Vec<CapturedRequest>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取抓包文件失败：{}", path.display()))?;
    let mut requests = parse_capture_text(&text)?;
    if requests.is_empty() {
        bail!("抓包里没有找到可用的请求");
    }
    sort_by_time(&mut requests);
    Ok(requests)
}

fn parse_capture_text(text: &str) -> Result<Vec<CapturedRequest>> {
    let text = text.trim_start_matches('\u{feff}');
    let trimmed = text.trim_start();
    // 完整 JSON 优先；失败时再按 JSONL（每行一个对象）解析。
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(root) = serde_json::from_str::<Value>(text) {
            if let Some(entries) = root.pointer("/log/entries").and_then(Value::as_array) {
                return Ok(entries
                    .iter()
                    .enumerate()
                    .filter_map(|(index, entry)| {
                        let mut request = parse_request(index, entry.get("request")?)?;
                        request.started_at = entry
                            .get("startedDateTime")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        Some(request)
                    })
                    .collect());
            }
            if let Some(items) = root.as_array() {
                return Ok(items
                    .iter()
                    .enumerate()
                    .filter_map(|(index, item)| parse_request(index, item))
                    .collect());
            }
            if root.get("request").is_some() {
                return Ok(parse_request(0, &root).into_iter().collect());
            }
            bail!("无法识别的抓包格式：需要 HAR 或请求数组");
        }
    }
    // JSONL / NDJSON：每行一个请求对象。
    let mut requests = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .with_context(|| format!("解析第 {} 行 JSONL 失败", index + 1))?;
        if let Some(request) = parse_request(index, &value) {
            requests.push(request);
        }
    }
    if requests.is_empty() {
        bail!("解析抓包失败：需要 HAR、请求数组或 JSONL");
    }
    Ok(requests)
}

fn parse_request(index: usize, value: &Value) -> Option<CapturedRequest> {
    let method = value.get("method")?.as_str()?.to_ascii_uppercase();
    let url = value.get("url")?.as_str()?.to_owned();
    let headers = parse_headers(value.get("headers"));
    let body = value
        .pointer("/postData/text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| value.get("body").and_then(Value::as_str).map(str::to_owned));
    Some(CapturedRequest {
        order: index,
        started_at: None,
        method,
        url,
        headers,
        body,
    })
}

fn parse_headers(value: Option<&Value>) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    match value {
        Some(Value::Array(items)) => {
            for item in items {
                let Some(name) = item.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let Some(value) = item.get("value").and_then(Value::as_str) else {
                    continue;
                };
                if !name.starts_with(':') {
                    headers.push((name.to_owned(), value.to_owned()));
                }
            }
        }
        Some(Value::Object(map)) => {
            for (name, value) in map {
                if let Some(value) = value.as_str()
                    && !name.starts_with(':')
                {
                    headers.push((name.clone(), value.to_owned()));
                }
            }
        }
        _ => {}
    }
    headers
}

fn sort_by_time(requests: &mut [CapturedRequest]) {
    requests.sort_by(|a, b| {
        let (a_time, b_time) = (parse_time(&a.started_at), parse_time(&b.started_at));
        // 有时间戳的排在前面；时间相同或都没有时间戳时保持抓包原顺序。
        a_time
            .is_none()
            .cmp(&b_time.is_none())
            .then_with(|| match (a_time, b_time) {
                (Some(a_time), Some(b_time)) => a_time.cmp(&b_time),
                _ => std::cmp::Ordering::Equal,
            })
            .then(a.order.cmp(&b.order))
    });
}

fn parse_time(value: &Option<String>) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(value.as_deref()?).ok()
}

/// 重放参数。
#[derive(Debug, Clone)]
pub struct ReplayOptions {
    pub media: MediaKind,
    pub object_id: Option<String>,
    pub jobid: Option<String>,
    pub limit: Option<usize>,
    pub delay: Duration,
    pub dry_run: bool,
    /// 收到 `isPassed=true` 后停止。
    pub stop_on_passed: bool,
    /// 单条报文失败后继续发送后续报文。
    pub continue_on_error: bool,
    /// 把抓包里的 `_t` 缓存参数刷新为当前时间。
    pub refresh_t: bool,
    /// 直接指定 Cookie 头，优先级最高。
    pub cookie: Option<String>,
    /// 即使 cookies.txt 里有 Cookie，也优先使用抓包里的 Cookie。
    pub prefer_capture_cookies: bool,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            media: MediaKind::All,
            object_id: None,
            jobid: None,
            limit: None,
            delay: Duration::ZERO,
            dry_run: false,
            stop_on_passed: true,
            continue_on_error: false,
            refresh_t: true,
            cookie: None,
            prefer_capture_cookies: false,
        }
    }
}

/// 从抓包中挑出要重放的媒体进度报文，保持抓包时间顺序。
pub fn select<'a>(
    requests: &'a [CapturedRequest],
    options: &ReplayOptions,
) -> Vec<&'a CapturedRequest> {
    requests
        .iter()
        .filter(|request| request.is_media_log())
        .filter(|request| options.media.matches(request.dtype().as_deref()))
        .filter(|request| {
            options
                .object_id
                .as_deref()
                .is_none_or(|id| request.query("objectId").as_deref() == Some(id))
        })
        .filter(|request| {
            options
                .jobid
                .as_deref()
                .is_none_or(|id| request.query("jobid").as_deref() == Some(id))
        })
        .take(options.limit.unwrap_or(usize::MAX))
        .collect()
}

/// 检查 `enc` 是否与报文参数一致。抓包被改动或参数被重写时给出提示。
pub fn enc_mismatch(request: &CapturedRequest) -> Option<String> {
    let clazz_id = request.query("clazzId")?;
    let uid = request.query("userid")?;
    let jobid = request.query("jobid")?;
    let object_id = request.query("objectId")?;
    let playing_time = request.playing_time()?;
    let duration = request.duration()?;
    let enc = request.query("enc")?;
    let expected = video_log_enc(&clazz_id, &uid, &jobid, &object_id, playing_time, duration);
    (expected != enc).then(|| {
        format!(
            "enc 与报文参数不一致（抓包 enc={enc}，按参数计算={expected}），服务器可能拒绝这条报文"
        )
    })
}

/// 重放结果。
#[derive(Debug, Clone, Default)]
pub struct ReplayOutcome {
    /// 已发送（dry-run 时为已列出）的报文数。
    pub sent: usize,
    /// 服务器是否确认任务完成。
    pub passed: bool,
    /// 是否因为 `isPassed=true` 提前停止。
    pub stopped_on_passed: bool,
}

/// 按顺序发送抓到的报文。
pub async fn replay(
    http: &Http,
    requests: &[&CapturedRequest],
    options: &ReplayOptions,
) -> Result<ReplayOutcome> {
    if requests.is_empty() {
        bail!("抓包里没有匹配的媒体进度报文");
    }
    let use_store_cookie =
        options.cookie.is_none() && !options.prefer_capture_cookies && http.uid().is_ok();
    // 使用抓包/Cookie 头时不能再让 reqwest 的 Cookie 罐追加一次 Cookie：
    // 否则请求里会出现两个 Cookie 头，真正的抓包 Cookie 可能被覆盖。
    let cookie_free = if use_store_cookie {
        None
    } else {
        Some(Http::empty()?)
    };
    let client = cookie_free
        .as_ref()
        .map(|http| &http.client)
        .unwrap_or(&http.client);
    let mut outcome = ReplayOutcome::default();
    let total = requests.len();
    for (index, request) in requests.iter().enumerate() {
        if index > 0 && !options.delay.is_zero() && !options.dry_run {
            tokio::time::sleep(options.delay).await;
        }
        if options.dry_run {
            tracing::info!(
                "[计划 {}/{}] {} {} dtype={}",
                index + 1,
                total,
                request.method,
                request.progress_label(),
                request.dtype().as_deref().unwrap_or("?")
            );
            outcome.sent += 1;
            continue;
        }
        match send_one(client, request, options, use_store_cookie).await {
            Ok(passed) => {
                outcome.sent += 1;
                tracing::info!(
                    "[{}/{}] {} {} dtype={} -> isPassed={passed}",
                    index + 1,
                    total,
                    request.method,
                    request.progress_label(),
                    request.dtype().as_deref().unwrap_or("?"),
                );
                if passed {
                    outcome.passed = true;
                    if options.stop_on_passed {
                        outcome.stopped_on_passed = true;
                        break;
                    }
                }
            }
            Err(error) => {
                if error
                    .downcast_ref::<crate::error::VerificationRequired>()
                    .is_some()
                {
                    return Err(error);
                }
                if options.continue_on_error {
                    tracing::warn!("[{}/{}] 发送失败，继续下一条：{error}", index + 1, total);
                    outcome.sent += 1;
                    continue;
                }
                return Err(error);
            }
        }
    }
    Ok(outcome)
}

async fn send_one(
    client: &reqwest::Client,
    request: &CapturedRequest,
    options: &ReplayOptions,
    use_store_cookie: bool,
) -> Result<bool> {
    let url = replay_url(request, options.refresh_t)?;
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|_| anyhow!("抓包中的 HTTP 方法无效：{}", request.method))?;
    let mut builder = client.request(method, url);
    for (name, value) in &request.headers {
        if is_replay_header(name) {
            builder = builder.header(name, value);
        }
    }
    let cookie = options.cookie.as_deref().or_else(|| {
        (!use_store_cookie)
            .then(|| request.header("cookie"))
            .flatten()
    });
    if let Some(cookie) = cookie {
        builder = builder.header(reqwest::header::COOKIE, cookie);
    }
    if let Some(body) = &request.body {
        builder = builder.body(body.clone());
    }
    let response = builder.send().await.context("发送重放报文失败")?;
    let status = response.status();
    let response_url = response.url().to_string();
    let body = response.text().await.context("读取重放响应失败")?;
    if let Some(error) = verification_required(&response_url, &body) {
        return Err(error);
    }
    if status == reqwest::StatusCode::FORBIDDEN || body.contains("验证码") {
        tracing::warn!(
            "重放报文触发验证：{response_url}；响应片段：{}",
            crate::http::snippet(&body, 240)
        );
        return Err(crate::error::VerificationRequired.into());
    }
    if !status.is_success() {
        bail!(
            "媒体进度上报失败：HTTP {status} {}",
            body.chars().take(120).collect::<String>()
        );
    }
    let passed = match serde_json::from_str::<Value>(&body) {
        Ok(value) => value
            .get("isPassed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        Err(_) => {
            tracing::warn!(
                "响应不是 JSON，无法判断是否完成：{}",
                body.chars().take(120).collect::<String>()
            );
            false
        }
    };
    Ok(passed)
}

/// 只转发与媒体进度播放相关的请求头，避免把 `Host`、`Content-Length`
/// 等由客户端重新计算的字段原样带过去。
fn is_replay_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "referer"
            | "origin"
            | "user-agent"
            | "accept"
            | "accept-language"
            | "x-requested-with"
            | "content-type"
    )
}

/// 刷新 `_t`，其余查询参数保持原顺序。`enc` 不包含 `_t`，刷新不影响签名。
fn replay_url(request: &CapturedRequest, refresh_t: bool) -> Result<String> {
    if !refresh_t {
        return Ok(request.url.clone());
    }
    let mut url =
        Url::parse(&request.url).with_context(|| format!("抓包中的 URL 无效：{}", request.url))?;
    if !url.query_pairs().any(|(key, _)| key == "_t") {
        return Ok(url.to_string());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    {
        let mut query = url.query_pairs_mut();
        query.clear();
        for (key, value) in pairs {
            if key == "_t" {
                query.append_pair(&key, &now);
            } else {
                query.append_pair(&key, &value);
            }
        }
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::http::Http;
    use crate::testutil::{MockResponse, MockServer};

    fn media_url(base: &str, dtype: &str, playing_time: i64, enc: &str) -> String {
        format!(
            "{base}/mooc-ans/multimedia/log/a/1234/dtoken?clazzId=1&playingTime={playing_time}\
             &duration=600&clipTime=0_600&objectId=obj&otherInfo=nodeId_1-cpi_1-rt_d\
             &courseId=2&jobid=job&userid=99&isdrag=3&view=pc&enc={enc}&dtype={dtype}\
             &rt=0.9&_t=1700000000000"
        )
    }

    fn captured(index: usize, url: String) -> CapturedRequest {
        CapturedRequest {
            order: index,
            started_at: None,
            method: "GET".into(),
            url,
            headers: vec![
                (
                    "Referer".into(),
                    "https://mooc1.chaoxing.com/ananas/modules/audio/index_new.html".into(),
                ),
                ("Cookie".into(), "_uid=99; fid=1024".into()),
            ],
            body: None,
        }
    }

    #[test]
    fn har_is_sorted_by_time_and_selected_by_media_kind() {
        let har = serde_json::json!({
            "log": {
                "version": "1.2",
                "entries": [
                    {
                        "startedDateTime": "2026-11-05T10:00:30.000Z",
                        "request": {
                            "method": "GET",
                            "url": media_url("https://mooc1.chaoxing.com", "Audio", 30, "a"),
                            "headers": []
                        }
                    },
                    {
                        "startedDateTime": "2026-11-05T10:00:10.000Z",
                        "request": {
                            "method": "GET",
                            "url": "https://mooc1.chaoxing.com/ananas/status/obj",
                            "headers": []
                        }
                    },
                    {
                        "startedDateTime": "2026-11-05T10:00:00.000Z",
                        "request": {
                            "method": "GET",
                            "url": media_url("https://mooc1.chaoxing.com", "Audio", 0, "b"),
                            "headers": []
                        }
                    }
                ]
            }
        });
        let mut requests = parse_capture_text(&har.to_string()).unwrap();
        sort_by_time(&mut requests);

        let all = select(&requests, &ReplayOptions::default());
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].playing_time(), Some(0));
        assert_eq!(all[1].playing_time(), Some(30));

        let audio = select(
            &requests,
            &ReplayOptions {
                media: MediaKind::Audio,
                ..ReplayOptions::default()
            },
        );
        assert_eq!(audio.len(), 2);

        let video = select(
            &requests,
            &ReplayOptions {
                media: MediaKind::Video,
                ..ReplayOptions::default()
            },
        );
        assert!(video.is_empty());
    }

    #[test]
    fn jsonl_capture_and_filters_are_supported() {
        let line = |playing_time: i64, jobid: &str| {
            serde_json::json!({
                "method": "GET",
                "url": media_url("https://mooc1.chaoxing.com", "Audio", playing_time, "x")
                    .replace("jobid=job", &format!("jobid={jobid}")),
            })
            .to_string()
        };
        let text = format!("{}\n{}\n", line(0, "a"), line(30, "b"));
        let requests = parse_capture_text(&text).unwrap();
        let options = ReplayOptions {
            jobid: Some("b".into()),
            ..ReplayOptions::default()
        };
        let selected = select(&requests, &options);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].playing_time(), Some(30));
    }

    #[test]
    fn refreshing_t_keeps_signature_and_other_params() {
        let request = captured(0, media_url("https://mooc1.chaoxing.com", "Audio", 30, "x"));
        let url = replay_url(&request, true).unwrap();
        let parsed = Url::parse(&url).unwrap();
        let pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        let get = |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_ne!(get("_t"), Some("1700000000000"));
        assert_eq!(get("playingTime"), Some("30"));
        assert_eq!(get("objectId"), Some("obj"));
        assert_eq!(get("otherInfo"), Some("nodeId_1-cpi_1-rt_d"));
    }

    #[test]
    fn enc_mismatch_is_reported() {
        let valid = video_log_enc("1", "99", "job", "obj", 60, 600);
        let request = captured(
            0,
            media_url("https://mooc1.chaoxing.com", "Audio", 60, &valid),
        );
        assert!(enc_mismatch(&request).is_none());

        let tampered = captured(
            0,
            media_url("https://mooc1.chaoxing.com", "Audio", 61, &valid),
        );
        assert!(enc_mismatch(&tampered).is_some());
    }

    #[tokio::test]
    async fn replay_sends_in_order_and_stops_when_passed() {
        let server = MockServer::start(vec![
            MockResponse::json(200, serde_json::json!({"isPassed": false})),
            MockResponse::json(200, serde_json::json!({"isPassed": false})),
            MockResponse::json(200, serde_json::json!({"isPassed": true})),
        ])
        .await;
        let http = Http::new("/nonexistent/cookies.txt").unwrap();
        let requests: Vec<CapturedRequest> = (0..5)
            .map(|index| {
                captured(
                    index,
                    media_url(&server.url, "Audio", index as i64 * 30, "x"),
                )
            })
            .collect();
        let selected: Vec<&CapturedRequest> = requests.iter().collect();
        let options = ReplayOptions::default();

        let outcome = replay(&http, &selected, &options).await.unwrap();

        assert!(outcome.passed);
        assert!(outcome.stopped_on_passed);
        assert_eq!(outcome.sent, 3);
        let recorded = server.requests();
        assert_eq!(recorded.len(), 3);
        assert!(recorded[0].path.contains("playingTime=0"));
        assert!(recorded[1].path.contains("playingTime=30"));
        assert!(recorded[2].path.contains("playingTime=60"));
        assert_eq!(
            recorded[0].header("referer"),
            Some("https://mooc1.chaoxing.com/ananas/modules/audio/index_new.html")
        );
        assert_eq!(recorded[0].header("cookie"), Some("_uid=99; fid=1024"));
    }

    #[tokio::test]
    async fn captured_cookie_is_used_without_cookie_jar_override() {
        let cookie_path = std::env::temp_dir().join(format!(
            "fkchaoxing-replay-cookie-{}.txt",
            std::process::id()
        ));
        std::fs::write(&cookie_path, "_uid=store; fid=1").unwrap();
        let server = MockServer::start(vec![MockResponse::json(
            200,
            serde_json::json!({"isPassed": true}),
        )])
        .await;
        let http = Http::new(&cookie_path).unwrap();
        let request = captured(0, media_url(&server.url, "Audio", 0, "x"));
        let options = ReplayOptions {
            prefer_capture_cookies: true,
            ..ReplayOptions::default()
        };

        replay(&http, &[&request], &options).await.unwrap();

        let recorded = server.requests();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].header("cookie"), Some("_uid=99; fid=1024"));
        let _ = std::fs::remove_file(cookie_path);
    }

    #[tokio::test]
    async fn dry_run_lists_without_waiting_or_sending() {
        let http = Http::new("/nonexistent/cookies.txt").unwrap();
        let requests: Vec<CapturedRequest> = (0..3)
            .map(|index| {
                captured(
                    index,
                    media_url(
                        "https://mooc1.chaoxing.com",
                        "Audio",
                        index as i64 * 30,
                        "x",
                    ),
                )
            })
            .collect();
        let selected: Vec<&CapturedRequest> = requests.iter().collect();
        let options = ReplayOptions {
            dry_run: true,
            delay: Duration::from_secs(10),
            ..ReplayOptions::default()
        };

        let started = std::time::Instant::now();
        let outcome = replay(&http, &selected, &options).await.unwrap();

        assert_eq!(outcome.sent, 3);
        assert!(!outcome.passed);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn replay_keeps_going_when_continue_on_error_is_set() {
        let server = MockServer::start(vec![
            MockResponse::json(500, serde_json::json!({"error": "boom"})),
            MockResponse::json(200, serde_json::json!({"isPassed": true})),
        ])
        .await;
        let http = Http::new("/nonexistent/cookies.txt").unwrap();
        let requests: Vec<CapturedRequest> = (0..2)
            .map(|index| {
                captured(
                    index,
                    media_url(&server.url, "Audio", index as i64 * 30, "x"),
                )
            })
            .collect();
        let selected: Vec<&CapturedRequest> = requests.iter().collect();
        let options = ReplayOptions {
            continue_on_error: true,
            ..ReplayOptions::default()
        };

        let outcome = replay(&http, &selected, &options).await.unwrap();

        assert!(outcome.passed);
        assert_eq!(outcome.sent, 2);
        assert_eq!(server.requests().len(), 2);
    }
}
