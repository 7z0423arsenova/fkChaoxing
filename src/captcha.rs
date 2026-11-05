//! 学习通反爬图片验证码自动识别。
//!
//! 触发风控时，学习通会把请求重定向到 `antispiderShowVerify.ac`。此时可以：
//!
//! 1. `GET https://mooc1-api.chaoxing.com/processVerifyPng.ac` 获取验证码图片；
//! 2. 用 ddddocr 识别图片字符；
//! 3. `POST https://mooc1-api.chaoxing.com/html/processVerify.ac`
//!    提交 `app=0&ucode=<识别结果>`，返回 3xx 表示通过；
//! 4. 重新发送刚才被拦截的请求。
//!
//! 这里只负责第 1~3 步，重发原请求由 [`crate::client::ChaoxingClient`] 完成。

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::StatusCode;
use reqwest::header::REFERER;
use tracing::{debug, info, warn};

use crate::config::Section;
use crate::http::{Http, snippet};
use crate::ocr::{DdddOcr, OcrBackend};

const DEFAULT_ENDPOINT: &str = "https://mooc1-api.chaoxing.com";
const DEFAULT_MAX_RETRIES: u32 = 6;
const DEFAULT_DELAY_SECS: f64 = 5.0;
const DEFAULT_TIMEOUT_SECS: f64 = 30.0;
/// 识别时使用的来源页，贴近浏览器里被拦截的课程页面。
const CAPTCHA_REFERER: &str = "https://mooc1.chaoxing.com/";

/// `[captcha]` 节的配置。
#[derive(Debug, Clone)]
pub struct CaptchaConfig {
    pub enabled: bool,
    /// Python 解释器；留空时依次尝试 python3、python。
    pub python: String,
    /// 验证码服务地址，通常不需要修改。
    pub endpoint: String,
    /// 单次验证最多识别几次。
    pub max_retries: u32,
    /// 每次识别前的等待时间，给服务端留出恢复时间。
    pub delay: Duration,
    /// ddddocr 启动和单次识别的超时时间。
    pub timeout: Duration,
}

impl Default for CaptchaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            python: String::new(),
            endpoint: DEFAULT_ENDPOINT.to_owned(),
            max_retries: DEFAULT_MAX_RETRIES,
            delay: Duration::from_secs_f64(DEFAULT_DELAY_SECS),
            timeout: Duration::from_secs_f64(DEFAULT_TIMEOUT_SECS),
        }
    }
}

impl CaptchaConfig {
    pub fn from_section(section: &Section) -> Self {
        let mut config = Self::default();
        config.enabled = section.bool_or("enabled", config.enabled);
        config.python = section.get_or("python", "").to_owned();
        if let Some(endpoint) = section.get("endpoint") {
            config.endpoint = endpoint.trim_end_matches('/').to_owned();
        }
        if let Some(retries) = section.get_u64("max_retries") {
            config.max_retries = retries.clamp(1, 20) as u32;
        }
        if let Some(delay) = section.get_f64("delay") {
            config.delay = Duration::from_secs_f64(delay.max(0.0));
        }
        if let Some(timeout) = section.get_f64("timeout") {
            config.timeout = Duration::from_secs_f64(timeout.max(1.0));
        }
        config
    }

    pub fn build(&self) -> Option<Arc<CaptchaSolver>> {
        self.enabled.then(|| {
            info!(
                "已启用 ddddocr 验证码识别：{}（最多 {} 次）",
                self.endpoint, self.max_retries
            );
            Arc::new(CaptchaSolver::with_ocr(
                Arc::new(DdddOcr::new(self.python.clone(), self.timeout)),
                self.endpoint.clone(),
                self.max_retries,
                self.delay,
            ))
        })
    }
}

/// 串行处理验证码的求解器（同一会话不需要并发识别）。
pub struct CaptchaSolver {
    ocr: Arc<dyn OcrBackend>,
    endpoint: String,
    max_retries: u32,
    delay: Duration,
    lock: tokio::sync::Mutex<()>,
}

/// 一次验证尝试的结果。
enum Attempt {
    /// 已提交且服务器返回通过。
    Passed,
    /// 已提交，但服务器拒绝。
    Rejected,
    /// 没有拿到图片验证码（通常是 404/410，地址或会话可能不对）。
    NoImage,
}

impl CaptchaSolver {
    /// 使用指定 OCR 后端构造求解器；主要供测试注入假实现。
    pub fn with_ocr(
        ocr: Arc<dyn OcrBackend>,
        endpoint: impl Into<String>,
        max_retries: u32,
        delay: Duration,
    ) -> Self {
        Self {
            ocr,
            endpoint: endpoint.into().trim_end_matches('/').to_owned(),
            max_retries: max_retries.clamp(1, 20),
            delay,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 完成一次验证码验证。成功后，原被拦截的请求可以重新发送。
    pub async fn solve(&self, http: &Http) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut last_error = None;
        for attempt in 1..=self.max_retries {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            match self.attempt(http).await {
                Ok(Attempt::Passed) => {
                    info!("验证码已通过（第 {attempt} 次识别）");
                    return Ok(());
                }
                Ok(Attempt::Rejected) => {
                    warn!("验证码未通过（{attempt}/{}），继续重试", self.max_retries);
                }
                Ok(Attempt::NoImage) => {
                    // 404/410 不代表验证已通过；地址或会话不对时不能盲目重发原请求。
                    return Err(anyhow!(
                        "验证码图片接口没有返回图片，无法确认验证状态；请检查 [captcha] endpoint 或当前会话"
                    ));
                }
                Err(error) => {
                    warn!(
                        "验证码识别失败（{attempt}/{}）：{error:#}",
                        self.max_retries
                    );
                    last_error = Some(error);
                }
            }
        }
        match last_error {
            Some(error) => Err(error.context(format!(
                "验证码连续 {} 次识别失败，请检查 ddddocr 安装",
                self.max_retries
            ))),
            None => bail!("验证码连续 {} 次未通过", self.max_retries),
        }
    }

    /// 返回本次验证的结果；错误表示 OCR 或网络失败。
    async fn attempt(&self, http: &Http) -> Result<Attempt> {
        let Some(image) = self.fetch_image(http).await? else {
            // 图片接口已经拿不到验证码，说明验证状态可能已被解除。
            return Ok(Attempt::NoImage);
        };
        let code = self.ocr.recognize(&image).await?;
        if code.trim().is_empty() {
            bail!("验证码识别结果为空");
        }
        debug!("ddddocr 识别结果：{code}");
        if self.submit(http, &code).await? {
            Ok(Attempt::Passed)
        } else {
            Ok(Attempt::Rejected)
        }
    }

    async fn fetch_image(&self, http: &Http) -> Result<Option<Vec<u8>>> {
        let url = format!("{}/processVerifyPng.ac", self.endpoint);
        let response = http
            .client
            .get(&url)
            .header(REFERER, CAPTCHA_REFERER)
            .query(&[("t", timestamp_ms())])
            .send()
            .await
            .context("获取验证码图片失败")?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
            warn!("验证码图片接口返回 {status}，无法确认验证状态");
            return Ok(None);
        }
        if !status.is_success() {
            bail!("获取验证码图片失败：HTTP {status}");
        }
        if let Some(content_type) = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            && !content_type.to_ascii_lowercase().starts_with("image/")
        {
            bail!("验证码接口返回的不是图片：{content_type}");
        }
        let bytes = response.bytes().await.context("读取验证码图片失败")?;
        if bytes.is_empty() {
            bail!("验证码图片为空");
        }
        debug!("已获取验证码图片（{} 字节）", bytes.len());
        Ok(Some(bytes.to_vec()))
    }

    async fn submit(&self, http: &Http, code: &str) -> Result<bool> {
        let client = http.no_redirect_client()?;
        let url = format!("{}/html/processVerify.ac", self.endpoint);
        let response = client
            .post(&url)
            .header(REFERER, CAPTCHA_REFERER)
            .form(&[("app", "0"), ("ucode", code)])
            .send()
            .await
            .context("提交验证码失败")?;
        let status = response.status();
        if status.is_redirection() {
            return Ok(true);
        }
        let body = response.text().await.unwrap_or_default();
        debug!(
            "验证码 {code} 未通过：HTTP {status}；响应片段：{}",
            snippet(&body, 160)
        );
        Ok(false)
    }
}

fn timestamp_ms() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ChaoxingClient, Endpoints};
    use crate::ocr::OcrBackend;
    use crate::testutil::{MockResponse, MockServer};
    use std::future::Future;
    use std::pin::Pin;

    struct FakeOcr(&'static str);

    impl OcrBackend for FakeOcr {
        fn recognize<'a>(
            &'a self,
            _image: &'a [u8],
        ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
            let code = self.0.to_owned();
            Box::pin(async move { Ok(code) })
        }
    }

    struct FailingOcr;

    impl OcrBackend for FailingOcr {
        fn recognize<'a>(
            &'a self,
            _image: &'a [u8],
        ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
            Box::pin(async { bail!("ddddocr 不可用") })
        }
    }

    struct PanicOcr;

    impl OcrBackend for PanicOcr {
        fn recognize<'a>(
            &'a self,
            _image: &'a [u8],
        ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
            Box::pin(async { panic!("没有图片时不应调用 OCR") })
        }
    }

    fn html(body: &str) -> MockResponse {
        MockResponse {
            status: 200,
            body: body.into(),
            headers: vec![("Content-Type".into(), "text/html".into())],
        }
    }

    fn image() -> MockResponse {
        MockResponse {
            status: 200,
            body: "fake-png".into(),
            headers: vec![("Content-Type".into(), "image/png".into())],
        }
    }

    #[test]
    fn parses_captcha_section() {
        let ini = crate::config::Ini::parse(
            "[captcha]\nenabled = true\npython = /opt/py\nmax_retries = 3\ndelay = 1.5\ntimeout = 12\nendpoint = https://example.com/\n",
        );
        let config = CaptchaConfig::from_section(ini.section("captcha").unwrap());
        assert!(config.enabled);
        assert_eq!(config.python, "/opt/py");
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.delay, Duration::from_millis(1500));
        assert_eq!(config.timeout, Duration::from_secs(12));
        assert_eq!(config.endpoint, "https://example.com");
    }

    #[test]
    fn captcha_is_disabled_by_default() {
        let config = CaptchaConfig::from_section(&Section::default());
        assert!(!config.enabled);
        assert!(config.build().is_none());
    }

    #[test]
    fn clamps_invalid_retry_count() {
        let ini = crate::config::Ini::parse("[captcha]\nmax_retries = 0\n");
        let config = CaptchaConfig::from_section(ini.section("captcha").unwrap());
        assert_eq!(config.max_retries, 1);
    }

    #[tokio::test]
    async fn solves_captcha_and_retries_original_request() {
        let server = MockServer::start(vec![
            html("【9010】操作异常，请输入图片中的验证码"),
            image(),
            MockResponse {
                status: 302,
                body: String::new(),
                headers: vec![("Location".into(), "/mooc-ans/knowledge/cards".into())],
            },
            html("正常内容"),
        ])
        .await;
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        let solver = CaptchaSolver::with_ocr(
            Arc::new(FakeOcr("abcd")),
            server.url.clone(),
            1,
            Duration::ZERO,
        );
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url))
            .with_captcha(Some(Arc::new(solver)));
        let request = client
            .http
            .client
            .get(format!("{}/mooc-ans/knowledge/cards", server.url));

        let body = client.fetch_text(request).await.unwrap();

        assert_eq!(body, "正常内容");
        let requests = server.requests();
        assert_eq!(requests.len(), 4);
        assert!(requests[1].path.contains("processVerifyPng.ac"));
        assert!(requests[2].path.contains("processVerify.ac"));
        assert!(requests[2].body.contains("ucode=abcd"));
        assert!(requests[3].path.contains("/mooc-ans/knowledge/cards"));
    }

    #[tokio::test]
    async fn missing_captcha_image_is_an_error_instead_of_success() {
        let server = MockServer::start(vec![
            html("请输入图片中的验证码"),
            MockResponse {
                status: 404,
                body: "not found".into(),
                headers: vec![],
            },
        ])
        .await;
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        let solver =
            CaptchaSolver::with_ocr(Arc::new(PanicOcr), server.url.clone(), 1, Duration::ZERO);
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url))
            .with_captcha(Some(Arc::new(solver)));
        let request = client.http.client.get(format!("{}/page", server.url));

        let error = client.fetch_text(request).await.unwrap_err();

        assert!(
            error
                .downcast_ref::<crate::error::VerificationRequired>()
                .is_some(),
            "{error:#}"
        );
        assert_eq!(server.requests().len(), 2);
    }

    #[tokio::test]
    async fn keeps_verification_error_when_ocr_fails() {
        let server = MockServer::start(vec![html("请输入图片中的验证码"), image()]).await;
        let http = Arc::new(Http::new("/nonexistent/cookies.txt").unwrap());
        let solver =
            CaptchaSolver::with_ocr(Arc::new(FailingOcr), server.url.clone(), 1, Duration::ZERO);
        let client = ChaoxingClient::new_with_endpoints(http, Endpoints::all(&server.url))
            .with_captcha(Some(Arc::new(solver)));
        let request = client.http.client.get(format!("{}/page", server.url));

        let error = client.fetch_text(request).await.unwrap_err();

        assert!(
            error
                .downcast_ref::<crate::error::VerificationRequired>()
                .is_some(),
            "{error:#}"
        );
    }
}
