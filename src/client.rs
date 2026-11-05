//! 学习通会话与课程发现接口。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tracing::warn;

use crate::captcha::CaptchaSolver;
use crate::crypto::encrypt_login_field;
use crate::decode::{
    parse_chapter_jobs, parse_course_folders, parse_course_list, parse_course_points,
};
use crate::http::{Http, RateLimiter, is_verification_page};
use crate::models::{Course, CoursePoints, Job, JobInfo, Point};

/// 普通学习通接口的全局最小请求间隔（秒）。
const REQUEST_INTERVAL: f64 = if cfg!(test) { 0.0 } else { 0.5 };
/// 任务卡片分页请求的间隔（秒），避免 7 次请求瞬时并发触发反爬。
const CARD_INTERVAL: f64 = if cfg!(test) { 0.0 } else { 0.15 };
/// 媒体进度上报的全局最小间隔（秒）。
const VIDEO_INTERVAL: f64 = if cfg!(test) { 0.0 } else { 2.0 };
/// 同一个请求因验证码最多自动重发几次。
const CAPTCHA_MAX_SOLVES: u32 = 3;
/// 自动重登成功后的去重窗口，避免并发章节连续重登。
const RELOGIN_OK_COOLDOWN: Duration = Duration::from_secs(5);
/// 自动重登失败后的等待时间，避免并发章节反复冲登录接口。
const RELOGIN_ERR_COOLDOWN: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Endpoints {
    pub passport: String,
    pub mooc2: String,
    pub mooc1: String,
    /// 签到接口使用；CLI 暂未提供签到命令。
    #[allow(dead_code)]
    pub mobile: String,
    pub zhibo: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            passport: "https://passport2.chaoxing.com".into(),
            mooc2: "https://mooc2-ans.chaoxing.com".into(),
            mooc1: "https://mooc1.chaoxing.com".into(),
            mobile: "https://mobilelearn.chaoxing.com".into(),
            zhibo: "https://zhibo.chaoxing.com".into(),
        }
    }
}

impl Endpoints {
    #[cfg(test)]
    pub(crate) fn all(url: &str) -> Self {
        Self {
            passport: url.into(),
            mooc2: url.into(),
            mooc1: url.into(),
            mobile: url.into(),
            zhibo: url.into(),
        }
    }
}

pub struct ChaoxingClient {
    pub http: Arc<Http>,
    pub endpoints: Endpoints,
    request_limiter: RateLimiter,
    card_limiter: RateLimiter,
    video_limiter: RateLimiter,
    /// 启动时使用的账号密码，供运行中 403/会话失效时自动重登。
    credentials: Mutex<Option<(String, String)>>,
    /// 上次自动重登成功/失败时间，避免多个并发章节同时重登或反复重试。
    last_relogin_ok: Mutex<Option<Instant>>,
    last_relogin_err: Mutex<Option<Instant>>,
    relogin_lock: tokio::sync::Mutex<()>,
    /// 可选的 ddddocr 验证码求解器；为 `None` 时保持原有“遇到验证码就停止”的行为。
    captcha: Option<Arc<CaptchaSolver>>,
}

/// 一次已读取完响应体的学习通请求。
pub struct Fetched {
    pub status: reqwest::StatusCode,
    pub url: String,
    pub body: String,
}

/// 发送请求；遇到验证页时用指定会话尝试自动识别并重发。
///
/// 独立成函数是为了让登录/重登也能使用临时 Cookie 会话，而不必清空当前会话。
async fn fetch_with_captcha(
    http: &Http,
    captcha: Option<&CaptchaSolver>,
    request: reqwest::RequestBuilder,
) -> Result<Fetched> {
    let mut builder = request;
    let mut solved = 0;
    loop {
        // 表单/查询参数请求都能克隆；流式请求体无法重放，保持原错误。
        let retry = builder.try_clone();
        let response = builder.send().await.context("发送学习通请求失败")?;
        let status = response.status();
        let url = response.url().to_string();
        let body = response.text().await.context("读取学习通响应失败")?;
        if !is_verification_page(&url, &body) {
            return Ok(Fetched { status, url, body });
        }
        if let Some(captcha) = captcha
            && solved < CAPTCHA_MAX_SOLVES
        {
            let Some(retry) = retry else {
                bail!("学习通要求验证码，但该请求无法重放");
            };
            warn!("请求触发学习通验证码，尝试用 ddddocr 自动识别：{url}");
            match captcha.solve(http).await {
                Ok(()) => {
                    solved += 1;
                    builder = retry;
                    continue;
                }
                Err(error) => warn!("ddddocr 自动识别验证码失败：{error:#}"),
            }
        }
        return Err(crate::http::verification_required(&url, &body)
            .unwrap_or_else(|| crate::error::VerificationRequired.into()));
    }
}

impl ChaoxingClient {
    pub fn new(http: Arc<Http>) -> Self {
        Self::new_with_endpoints(http, Endpoints::default())
    }

    pub fn new_with_endpoints(http: Arc<Http>, endpoints: Endpoints) -> Self {
        Self {
            http,
            endpoints,
            request_limiter: RateLimiter::new(REQUEST_INTERVAL),
            card_limiter: RateLimiter::new(CARD_INTERVAL),
            video_limiter: RateLimiter::new(VIDEO_INTERVAL),
            credentials: Mutex::new(None),
            last_relogin_ok: Mutex::new(None),
            last_relogin_err: Mutex::new(None),
            relogin_lock: tokio::sync::Mutex::new(()),
            captcha: None,
        }
    }

    /// 配置 ddddocr 验证码自动识别；不调用时保持原有行为。
    pub fn with_captcha(mut self, captcha: Option<Arc<CaptchaSolver>>) -> Self {
        self.captcha = captcha;
        self
    }

    /// 普通学习通接口的全局限速。
    pub async fn wait_request(&self) {
        self.request_limiter.wait().await;
    }

    /// 任务卡片分页请求的限速。
    pub async fn wait_card(&self) {
        self.card_limiter.wait().await;
    }

    /// 媒体进度上报的全局限速。
    pub async fn wait_video(&self) {
        self.video_limiter.wait().await;
    }

    /// 发送普通学习通请求，读取完整响应：统一先经过全局限速。
    ///
    /// 遇到图片验证码时会调用 ddddocr 自动识别并重发原请求。新增接口时优先
    /// 走这个入口；卡片、媒体等已有独立限速的路径先调用 [`Self::wait_card`]
    /// 或 [`Self::wait_video`]，再调用 [`Self::fetch_now`]。
    pub async fn fetch(&self, request: reqwest::RequestBuilder) -> Result<Fetched> {
        self.wait_request().await;
        self.fetch_now(request).await
    }

    /// 与 [`Self::fetch`] 相同，但不额外等待全局限速（用于卡片、媒体等已有独立限速的路径）。
    pub async fn fetch_now(&self, request: reqwest::RequestBuilder) -> Result<Fetched> {
        fetch_with_captcha(self.http.as_ref(), self.captcha.as_deref(), request).await
    }

    /// 发送请求并返回文本；非 2xx 状态返回错误。
    pub async fn fetch_text(&self, request: reqwest::RequestBuilder) -> Result<String> {
        let fetched = self.fetch(request).await?;
        if !fetched.status.is_success() {
            bail!("HTTP {}", fetched.status);
        }
        Ok(fetched.body)
    }

    /// 与 [`Self::fetch_text`] 相同，但不额外等待全局限速。
    pub async fn fetch_text_now(&self, request: reqwest::RequestBuilder) -> Result<String> {
        let fetched = self.fetch_now(request).await?;
        if !fetched.status.is_success() {
            bail!("HTTP {}", fetched.status);
        }
        Ok(fetched.body)
    }

    pub async fn login(&self, use_cookies: bool, username: &str, password: &str) -> Result<()> {
        if !username.trim().is_empty() && !password.is_empty() {
            *self.credentials.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((username.to_owned(), password.to_owned()));
        }
        if use_cookies && self.validate_session().await? {
            return Ok(());
        }
        if username.trim().is_empty() || password.is_empty() {
            bail!("Cookie 已失效，且未提供账号密码");
        }
        // 登录在独立 Cookie 会话里完成；只有登录和会话校验都成功才替换当前
        // Cookie。密码错误/验证码失败时，其他并发章节的旧会话仍然可用。
        let login_http = Http::empty()?;
        let endpoint = format!(
            "{}/fanyalogin",
            self.endpoints.passport.trim_end_matches('/')
        );
        let encrypted_username = encrypt_login_field(username);
        let encrypted_password = encrypt_login_field(password);
        let fields = [
            ("fid", "-1"),
            ("uname", encrypted_username.as_str()),
            ("password", encrypted_password.as_str()),
            ("refer", "https%3A%2F%2Fi.chaoxing.com"),
            ("t", "true"),
            ("forbidotherlogin", "0"),
            ("validate", ""),
            ("doubleFactorLogin", "0"),
            ("independentId", "0"),
        ];
        self.wait_request().await;
        let request = login_http.client.post(endpoint).form(&fields);
        let fetched = fetch_with_captcha(&login_http, self.captcha.as_deref(), request)
            .await
            .context("请求登录接口失败")?;
        if !fetched.status.is_success() {
            bail!("登录接口失败：HTTP {}", fetched.status);
        }
        let data: Value = serde_json::from_str(&fetched.body).context("解析登录响应失败")?;
        if data.get("status").and_then(Value::as_bool) != Some(true) {
            bail!(
                "登录失败: {}",
                data.get("msg2")
                    .and_then(Value::as_str)
                    .unwrap_or("接口未返回成功状态")
            );
        }
        if !self.validate_session_with(&login_http).await? {
            bail!("登录接口返回成功，但课程会话校验失败");
        }
        self.http.adopt_from(&login_http)?;
        Ok(())
    }

    /// 使用启动时保存的账号密码重新登录。
    ///
    /// 重登在独立 Cookie 会话中完成，失败不会影响当前会话。
    /// 成功后 5 秒内其他并发章节直接复用结果；失败后 10 秒内不重复尝试。
    pub async fn relogin(&self) -> Result<bool> {
        let _guard = self.relogin_lock.lock().await;
        {
            let ok = self
                .last_relogin_ok
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(at) = *ok
                && at.elapsed() < RELOGIN_OK_COOLDOWN
            {
                return Ok(true);
            }
        }
        {
            let err = self
                .last_relogin_err
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(at) = *err
                && at.elapsed() < RELOGIN_ERR_COOLDOWN
            {
                return Ok(false);
            }
        }
        let credentials = self
            .credentials
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some((username, password)) = credentials else {
            return Ok(false);
        };
        if username.trim().is_empty() || password.is_empty() {
            return Ok(false);
        }
        match self.login(false, &username, &password).await {
            Ok(()) => {
                *self
                    .last_relogin_ok
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                *self
                    .last_relogin_err
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
                Ok(true)
            }
            Err(error) => {
                *self
                    .last_relogin_err
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                Err(error)
            }
        }
    }

    async fn validate_session(&self) -> Result<bool> {
        self.validate_session_with(self.http.as_ref()).await
    }

    async fn validate_session_with(&self, http: &Http) -> Result<bool> {
        if http.cookie("_uid").is_none() && http.cookie("UID").is_none() {
            return Ok(false);
        }
        let url = format!(
            "{}/mooc2-ans/visit/courselistdata",
            self.endpoints.mooc2.trim_end_matches('/')
        );
        let request = http.client.post(url).form(&[
            ("courseType", "1"),
            ("courseFolderId", "0"),
            ("query", ""),
            ("superstarClass", "0"),
        ]);
        self.wait_request().await;
        let fetched = fetch_with_captcha(http, self.captcha.as_deref(), request)
            .await
            .context("校验课程会话失败")?;
        if !fetched.status.is_success() {
            return Ok(false);
        }
        // 只看最终地址和登录表单特征；课程名、脚本地址里出现 "login" 不能算未登录。
        let final_url = fetched.url.to_ascii_lowercase();
        if final_url.contains("passport2.chaoxing.com") || final_url.contains("/login") {
            return Ok(false);
        }
        let lower = fetched.body.to_ascii_lowercase();
        let looks_like_login = lower.contains("name=\"uname\"")
            || lower.contains("name='uname'")
            || lower.contains("id=\"uname\"")
            || lower.contains("id='uname'")
            || lower.contains("id=\"phone\"")
            || lower.contains("id='phone'")
            || lower.contains("fanyalogin");
        Ok(!looks_like_login)
    }

    pub async fn courses(&self) -> Result<Vec<Course>> {
        let base = self.endpoints.mooc2.trim_end_matches('/');
        let list_url = format!("{base}/mooc2-ans/visit/courselistdata");
        let referer = format!(
            "{base}/mooc2-ans/visit/interaction?moocDomain=https://mooc1-1.chaoxing.com/mooc-ans"
        );
        let mut courses = Vec::new();
        let root = self
            .http
            .client
            .post(&list_url)
            .header(reqwest::header::REFERER, referer)
            .form(&[
                ("courseType", "1"),
                ("courseFolderId", "0"),
                ("query", ""),
                ("superstarClass", "0"),
            ]);
        courses.extend(parse_course_list(
            &self.fetch_text(root).await.context("读取课程列表失败")?,
        ));

        let folders = self
            .http
            .client
            .get(format!("{base}/mooc2-ans/visit/interaction"));
        for folder in parse_course_folders(
            &self
                .fetch_text(folders)
                .await
                .context("读取课程文件夹失败")?,
        ) {
            let response = self.http.client.post(&list_url).form(&[
                ("courseType", "1"),
                ("courseFolderId", folder.id.as_str()),
                ("query", ""),
                ("superstarClass", "0"),
            ]);
            courses.extend(parse_course_list(
                &self
                    .fetch_text(response)
                    .await
                    .context("读取文件夹课程失败")?,
            ));
        }
        Ok(courses)
    }

    pub async fn course_points(&self, course: &Course) -> Result<CoursePoints> {
        let url = format!(
            "{}/mooc2-ans/mycourse/studentcourse",
            self.endpoints.mooc2.trim_end_matches('/')
        );
        let request = self.http.client.get(url).query(&[
            ("courseid", course.course_id.as_str()),
            ("clazzid", course.clazz_id.as_str()),
            ("cpi", course.cpi.as_str()),
            ("ut", "s"),
        ]);
        Ok(parse_course_points(
            &self.fetch_text(request).await.context("读取课程章节失败")?,
        ))
    }

    pub async fn chapter_jobs(
        &self,
        course: &Course,
        point: &Point,
    ) -> Result<(Vec<Job>, JobInfo)> {
        let url = format!(
            "{}/mooc-ans/knowledge/cards",
            self.endpoints.mooc1.trim_end_matches('/')
        );
        let mut jobs = Vec::new();
        let mut info = JobInfo::default();
        self.wait_request().await;
        for num in 0..=6 {
            self.wait_card().await;
            let num = num.to_string();
            let request = self.http.client.get(&url).query(&[
                ("clazzid", course.clazz_id.as_str()),
                ("courseid", course.course_id.as_str()),
                ("knowledgeid", point.id.as_str()),
                ("ut", "s"),
                ("cpi", course.cpi.as_str()),
                ("v", "2025-0424-1038-3"),
                ("mooc2", "1"),
                ("num", num.as_str()),
            ]);
            let fetched = self
                .fetch_now(request)
                .await
                .context("读取章节任务点失败")?;
            if !fetched.status.is_success() {
                bail!("读取章节任务点失败：HTTP {}", fetched.status);
            }
            let (mut page_jobs, page_info) = parse_chapter_jobs(&fetched.body);
            if page_info.not_open {
                // 第一页就未开放才是真的未开放；后续页出现该提示不能把已解析任务丢掉。
                if jobs.is_empty() {
                    return Ok((Vec::new(), page_info));
                }
                break;
            }
            if !page_info.present {
                // 第一页没有 mArg 通常是掉线、登录页或服务端异常，不能当成“没有任务”。
                if num == "0" {
                    bail!(
                        "任务点页面缺少 mArg（HTTP {}），无法确认章节任务，已放弃该章节",
                        fetched.status
                    );
                }
                break;
            }
            jobs.append(&mut page_jobs);
            info.merge(page_info);
        }
        Ok((jobs, info))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::error::VerificationRequired;
    use crate::testutil::{MockResponse, MockServer};

    fn text(body: &str) -> MockResponse {
        MockResponse {
            status: 200,
            body: body.into(),
            headers: vec![],
        }
    }

    #[tokio::test]
    async fn account_login_keeps_credentials_out_of_requests_and_saves_cookie() {
        let server = MockServer::start(vec![
            MockResponse::json(200, json!({"status": true}))
                .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse {
                status: 200,
                body: "course list".into(),
                headers: vec![],
            },
        ])
        .await;
        let cookie_path =
            std::env::temp_dir().join(format!("chaoxing-login-{}.txt", std::process::id()));
        let http = Arc::new(crate::http::Http::new(&cookie_path).unwrap());
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url));

        client.login(false, "13800000000", "secret").await.unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].path.starts_with("/fanyalogin"));
        assert!(!requests[0].body.contains("13800000000"));
        assert!(!requests[0].body.contains("secret"));
        assert_eq!(requests[1].header("Cookie"), Some("_uid=42"));
        assert!(
            std::fs::read_to_string(&cookie_path)
                .unwrap()
                .contains("_uid=42")
        );
        let _ = std::fs::remove_file(cookie_path);
    }

    #[tokio::test]
    async fn password_login_discards_stale_cookies() {
        let cookie_path =
            std::env::temp_dir().join(format!("chaoxing-stale-cookie-{}.txt", std::process::id()));
        std::fs::write(&cookie_path, "_uid=old; JSESSIONID=old-session").unwrap();
        let server = MockServer::start(vec![
            MockResponse::json(200, json!({"status": true}))
                .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse {
                status: 200,
                body: "course list".into(),
                headers: vec![],
            },
        ])
        .await;
        let http = Arc::new(Http::new(&cookie_path).unwrap());
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url));

        client.login(false, "13800000000", "secret").await.unwrap();

        let requests = server.requests();
        assert!(
            requests[0]
                .header("Cookie")
                .is_none_or(|cookie| !cookie.contains("old-session")),
            "登录请求不应携带旧会话: {:?}",
            requests[0].header("Cookie")
        );
        let _ = std::fs::remove_file(cookie_path);
    }

    #[tokio::test]
    async fn validate_session_ignores_login_word_in_course_html() {
        let cookie_path =
            std::env::temp_dir().join(format!("chaoxing-validate-{}.txt", std::process::id()));
        std::fs::write(&cookie_path, "_uid=42; fid=1024").unwrap();
        let server = MockServer::start(vec![text(
            r#"<script src="/mooc-ans/js/login.js"></script><span>login 课程</span>"#,
        )])
        .await;
        let http = Arc::new(Http::new(&cookie_path).unwrap());
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url));

        assert!(client.validate_session().await.unwrap());
        let _ = std::fs::remove_file(cookie_path);
    }

    #[tokio::test]
    async fn validate_session_rejects_login_redirect() {
        let cookie_path = std::env::temp_dir().join(format!(
            "chaoxing-validate-redirect-{}.txt",
            std::process::id()
        ));
        std::fs::write(&cookie_path, "_uid=42; fid=1024").unwrap();
        let server = MockServer::start(vec![
            MockResponse {
                status: 302,
                body: String::new(),
                headers: vec![("Location".into(), "/login".into())],
            },
            text("登录页"),
        ])
        .await;
        let http = Arc::new(Http::new(&cookie_path).unwrap());
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url));

        assert!(!client.validate_session().await.unwrap());
        let _ = std::fs::remove_file(cookie_path);
    }

    #[tokio::test]
    async fn failed_login_keeps_existing_session() {
        let cookie_path =
            std::env::temp_dir().join(format!("chaoxing-failed-login-{}.txt", std::process::id()));
        std::fs::write(&cookie_path, "_uid=old; fid=1024").unwrap();
        let server = MockServer::start(vec![MockResponse::json(
            200,
            json!({"status": false, "msg2": "密码错误"}),
        )])
        .await;
        let http = Arc::new(Http::new(&cookie_path).unwrap());
        let client = ChaoxingClient::new_with_endpoints(http.clone(), Endpoints::all(&server.url));

        let error = client
            .login(false, "13800000000", "wrong-password")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("登录失败"), "{error:#}");
        assert_eq!(http.cookie("_uid").as_deref(), Some("old"));
        assert!(
            std::fs::read_to_string(&cookie_path)
                .unwrap()
                .contains("_uid=old")
        );
        let _ = std::fs::remove_file(cookie_path);
    }

    #[tokio::test]
    async fn failed_relogin_keeps_session_and_skips_immediate_retry() {
        let cookie_path = std::env::temp_dir().join(format!(
            "chaoxing-failed-relogin-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&cookie_path);
        let server = MockServer::start(vec![
            MockResponse::json(200, json!({"status": true}))
                .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse {
                status: 200,
                body: "course list".into(),
                headers: vec![],
            },
            MockResponse::json(200, json!({"status": false, "msg2": "密码错误"})),
        ])
        .await;
        let http = Arc::new(Http::new(&cookie_path).unwrap());
        let client = ChaoxingClient::new_with_endpoints(http.clone(), Endpoints::all(&server.url));

        client.login(false, "13800000000", "secret").await.unwrap();
        assert_eq!(http.cookie("_uid").as_deref(), Some("42"));

        let error = client.relogin().await.unwrap_err();
        assert!(error.to_string().contains("登录失败"), "{error:#}");
        // 重登失败不能把并发章节正在用的旧会话清掉。
        assert_eq!(http.cookie("_uid").as_deref(), Some("42"));

        let requests_before = server.requests().len();
        assert!(!client.relogin().await.unwrap());
        assert_eq!(server.requests().len(), requests_before);
        let _ = std::fs::remove_file(cookie_path);
    }

    #[tokio::test]
    async fn relogin_uses_saved_credentials() {
        let cookie_path =
            std::env::temp_dir().join(format!("chaoxing-relogin-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&cookie_path);
        let server = MockServer::start(vec![
            MockResponse::json(200, json!({"status": true}))
                .with_header("Set-Cookie", "_uid=42; Path=/"),
            MockResponse {
                status: 200,
                body: "course list".into(),
                headers: vec![],
            },
            MockResponse::json(200, json!({"status": true}))
                .with_header("Set-Cookie", "_uid=43; Path=/"),
            MockResponse {
                status: 200,
                body: "course list".into(),
                headers: vec![],
            },
        ])
        .await;
        let http = Arc::new(Http::new(&cookie_path).unwrap());
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url));

        client.login(false, "13800000000", "secret").await.unwrap();
        assert!(client.relogin().await.unwrap());

        let requests = server.requests();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[2].path, "/fanyalogin");
        assert_eq!(requests[3].path, "/mooc2-ans/visit/courselistdata");
        let saved = std::fs::read_to_string(&cookie_path).unwrap();
        assert!(saved.contains("_uid=43"), "{saved}");
        let _ = std::fs::remove_file(cookie_path);
    }

    #[tokio::test]
    async fn courses_include_root_and_folder_entries() {
        let course = |id: &str| {
            format!(
                r#"<div class="course" id="c{id}"><input class="clazzId" value="10"><input class="courseId" value="{id}"><a href="/visit?cpi=30&amp;x=1"></a><span class="course-name" title="Course {id}"></span></div>"#
            )
        };
        let server = MockServer::start(vec![
            text(&course("1")),
            text(r#"<ul class="file-list"><li fileid="77"><input class="rename-input" value="Folder"></li></ul>"#),
            text(&course("2")),
        ]).await;
        let http = Arc::new(crate::http::Http::new("/nonexistent/cookies.txt").unwrap());
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url));

        let courses = client.courses().await.unwrap();

        assert_eq!(
            courses
                .iter()
                .map(|course| course.course_id.as_str())
                .collect::<Vec<_>>(),
            ["1", "2"]
        );
        let requests = server.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests[2].body.contains("courseFolderId=77"));
    }

    #[tokio::test]
    async fn chapter_jobs_stop_when_chapter_is_closed() {
        let server = MockServer::start(vec![text("章节未开放")]).await;
        let http = Arc::new(crate::http::Http::new("/nonexistent/cookies.txt").unwrap());
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url));
        let course = crate::models::Course {
            course_id: "1".into(),
            clazz_id: "2".into(),
            cpi: "3".into(),
            ..Default::default()
        };
        let point = crate::models::Point {
            id: "4".into(),
            ..Default::default()
        };

        let (jobs, info) = client.chapter_jobs(&course, &point).await.unwrap();

        assert!(jobs.is_empty());
        assert!(info.not_open);
        assert_eq!(server.requests().len(), 1);
    }

    fn card_page(jobid: &str) -> String {
        format!(
            r#"<script>try{{ mArg = {{"attachments":[{{"type":"video","job":true,"jobid":"{jobid}","mid":1,"objectId":"obj","property":{{"name":"视频"}}}}],"defaults":{{"knowledgeid":"9","cpi":"30"}}}};}}catch(e){{}}</script>"#
        )
    }

    fn card_test_course() -> (Course, Point) {
        (
            Course {
                course_id: "10".into(),
                clazz_id: "20".into(),
                cpi: "30".into(),
                ..Default::default()
            },
            Point {
                id: "9".into(),
                ..Default::default()
            },
        )
    }

    #[tokio::test]
    async fn chapter_jobs_rejects_first_page_without_marg() {
        let server = MockServer::start(vec![text("<html>请登录</html>")]).await;
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url));
        let (course, point) = card_test_course();

        let error = client.chapter_jobs(&course, &point).await.unwrap_err();

        assert!(error.to_string().contains("缺少 mArg"), "{error:#}");
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn chapter_jobs_rejects_http_error() {
        let server = MockServer::start(vec![MockResponse {
            status: 500,
            body: "server error".into(),
            headers: vec![],
        }])
        .await;
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url));
        let (course, point) = card_test_course();

        let error = client.chapter_jobs(&course, &point).await.unwrap_err();

        assert!(error.to_string().contains("HTTP 500"), "{error:#}");
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn chapter_jobs_keeps_jobs_when_later_page_says_not_open() {
        let server = MockServer::start(vec![text(&card_page("v1")), text("章节未开放")]).await;
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url));
        let (course, point) = card_test_course();

        let (jobs, info) = client.chapter_jobs(&course, &point).await.unwrap();

        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].jobid, "v1");
        assert!(!info.not_open);
        assert_eq!(server.requests().len(), 2);
    }

    #[tokio::test]
    async fn chapter_jobs_keeps_jobs_when_later_page_has_no_marg() {
        let server = MockServer::start(vec![text(&card_page("v1")), text("<html></html>")]).await;
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url));
        let (course, point) = card_test_course();

        let (jobs, _) = client.chapter_jobs(&course, &point).await.unwrap();

        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].jobid, "v1");
        assert_eq!(server.requests().len(), 2);
    }

    #[tokio::test]
    async fn chapter_jobs_surfaces_antispider_page() {
        let server = MockServer::start(vec![text(
            "<html>【9010】操作异常，请输入图片中的验证码</html>",
        )])
        .await;
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url));
        let course = crate::models::Course {
            course_id: "1".into(),
            clazz_id: "2".into(),
            cpi: "3".into(),
            ..Default::default()
        };
        let point = crate::models::Point {
            id: "4".into(),
            ..Default::default()
        };

        let error = client.chapter_jobs(&course, &point).await.unwrap_err();

        assert!(
            error.downcast_ref::<VerificationRequired>().is_some(),
            "{error:#}"
        );
        assert_eq!(server.requests().len(), 1);
    }
}
