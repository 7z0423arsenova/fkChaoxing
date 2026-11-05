//! HTTP 会话：共享 Cookie、默认请求头与请求限速。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use cookie_store::{CookieStore, RawCookie};
use rand::Rng;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, redirect};
use reqwest_cookie_store::CookieStoreMutex;
use tracing::debug;
use url::Url;

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/118.0.0.0 Safari/537.36";
pub const VIDEO_REFERER: &str =
    "https://mooc1.chaoxing.com/ananas/modules/video/index.html?v=2025-0725-1842";
pub const AUDIO_REFERER: &str =
    "https://mooc1.chaoxing.com/ananas/modules/audio/index_new.html?v=2025-0725-1842";

/// 从 cookies.txt 载入的 cookie 作用于 chaoxing.com 及其所有子域名
const COOKIE_DOMAIN: &str = "chaoxing.com";
const COOKIE_BASE_URL: &str = "https://passport2.chaoxing.com/";

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 安装 rustls 加密后端（进程内只执行一次）。
pub fn ensure_tls_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// 学习通网页端默认请求头
pub fn browser_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    let pairs = [
        ("user-agent", USER_AGENT),
        (
            "sec-ch-ua",
            r#""Chromium";v="118", "Google Chrome";v="118", "Not=A?Brand";v="99""#,
        ),
        ("sec-ch-ua-mobile", "?0"),
        ("sec-ch-ua-platform", r#""Windows""#),
    ];
    for (k, v) in pairs {
        headers.insert(HeaderName::from_static(k), HeaderValue::from_static(v));
    }
    headers
}

/// 与学习通交互使用的 HTTP 会话。
pub struct Http {
    /// 默认客户端（自动跟随重定向）
    pub client: Client,
    store: Arc<CookieStoreMutex>,
    cookies_path: PathBuf,
}

impl Http {
    pub fn new(cookies_path: impl Into<PathBuf>) -> Result<Self> {
        let store = Arc::new(CookieStoreMutex::default());
        let http = Self {
            client: Self::build_client(store.clone())?,
            store,
            cookies_path: cookies_path.into(),
        };
        // 启动时先载入已保存的 cookies。
        http.load_cookies_file();
        Ok(http)
    }

    /// 创建一个不加载任何 Cookie 的独立会话。
    ///
    /// 登录/重登在独立会话里完成，只有成功后才把 Cookie 换到当前会话；
    /// 密码错误或验证码失败不会清掉其他并发章节正在使用的旧 Cookie。
    pub fn empty() -> Result<Self> {
        let store = Arc::new(CookieStoreMutex::default());
        Ok(Self {
            client: Self::build_client(store.clone())?,
            store,
            cookies_path: PathBuf::new(),
        })
    }

    /// 创建使用独立 Cookie 存储的客户端（登录时使用）。
    pub fn build_client(store: Arc<CookieStoreMutex>) -> Result<Client> {
        ensure_tls_provider();
        Client::builder()
            .default_headers(browser_headers())
            .cookie_provider(store)
            .redirect(redirect::Policy::limited(10))
            .timeout(DEFAULT_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .context("创建 HTTP 客户端失败")
    }

    fn store(&self) -> std::sync::MutexGuard<'_, CookieStore> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn cookie(&self, name: &str) -> Option<String> {
        self.store()
            .iter_unexpired()
            .find(|c| c.name() == name)
            .map(|c| c.value().to_owned())
    }

    /// 当前登录用户的 uid（`_uid` 或 `UID` cookie）。
    pub fn uid(&self) -> Result<String> {
        self.cookie("_uid")
            .or_else(|| self.cookie("UID"))
            .ok_or_else(|| anyhow!("无法从 cookie 中获取 uid，请重新登录"))
    }

    /// 学校 fid，缺省为 1024。
    pub fn fid(&self) -> String {
        self.cookie("fid").unwrap_or_else(|| "1024".to_owned())
    }

    /// 从 cookies 文件载入（格式 `k1=v1;k2=v2`），返回载入数量。
    pub fn load_cookies_file(&self) -> usize {
        let pairs = read_cookies_file(&self.cookies_path);
        let url = Url::parse(COOKIE_BASE_URL).expect("valid url");
        let mut store = self.store();
        for (name, value) in &pairs {
            let mut cookie = RawCookie::new(name.clone(), value.clone());
            cookie.set_domain(COOKIE_DOMAIN);
            cookie.set_path("/");
            if let Err(e) = store.insert_raw(&cookie, &url) {
                debug!("载入 cookie {name} 失败: {e}");
            }
        }
        pairs.len()
    }

    /// 登录成功后，用来源会话的 Cookie 替换当前会话并写入文件。
    ///
    /// 只有登录/校验都成功才会调用，因此失败时当前会话的 Cookie 不受影响。
    pub fn adopt_from(&self, source: &Http) -> Result<()> {
        let pairs: Vec<(String, String)> = {
            let store = source.store.lock().unwrap_or_else(|e| e.into_inner());
            store
                .iter_unexpired()
                .map(|cookie| (cookie.name().to_owned(), cookie.value().to_owned()))
                .collect()
        };
        let url = Url::parse(COOKIE_BASE_URL).expect("valid url");
        {
            let mut store = self.store();
            *store = CookieStore::default();
            for (name, value) in &pairs {
                let mut cookie = RawCookie::new(name.clone(), value.clone());
                cookie.set_domain(COOKIE_DOMAIN);
                cookie.set_path("/");
                if let Err(e) = store.insert_raw(&cookie, &url) {
                    debug!("采用登录 cookie {name} 失败: {e}");
                }
            }
        }
        save_cookies(&self.store, &self.cookies_path)
    }

    /// 使用同一 Cookie 存储、但不自动跟随重定向的客户端。
    ///
    /// 提交验证码时需要读取 302/202 等原始状态；默认客户端会自动跟随，
    /// 导致无法判断验证码是否通过。
    pub fn no_redirect_client(&self) -> Result<Client> {
        ensure_tls_provider();
        Client::builder()
            .default_headers(browser_headers())
            .cookie_provider(self.store.clone())
            .redirect(redirect::Policy::none())
            .timeout(DEFAULT_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .context("创建不跟随重定向的 HTTP 客户端失败")
    }
}

/// 读取 cookies 文件，返回 (名称, 值) 列表；文件不存在或无法读取时返回空。
pub fn read_cookies_file(path: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.trim()
        .split(';')
        .filter_map(|item| {
            let (k, v) = item.trim().split_once('=')?;
            let k = k.trim();
            (!k.is_empty()).then(|| (k.to_owned(), v.trim().to_owned()))
        })
        .collect()
}

/// 将 Cookie 存储中的所有有效 cookie 以 `k1=v1;k2=v2` 格式写入文件（同名只保留一个）。
pub fn save_cookies(store: &CookieStoreMutex, path: &Path) -> Result<()> {
    let store = store.lock().unwrap_or_else(|e| e.into_inner());
    let mut pairs: Vec<(String, String)> = Vec::new();
    for cookie in store.iter_unexpired() {
        match pairs.iter_mut().find(|(k, _)| k == cookie.name()) {
            Some(existing) => existing.1 = cookie.value().to_owned(),
            None => pairs.push((cookie.name().to_owned(), cookie.value().to_owned())),
        }
    }
    let buffer = pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";");
    // Cookies 是账号会话凭据：写入时收紧到 0600，并原子替换，避免半截文件。
    crate::fsutil::write_private(path, buffer.as_bytes())
        .with_context(|| format!("保存 cookies 到 {} 失败", path.display()))
}

/// 简单的请求限速器：保证相邻两次请求至少间隔 `interval`，可附加随机等待。
pub struct RateLimiter {
    interval: Duration,
    last: Mutex<Instant>,
}

impl RateLimiter {
    pub fn new(interval_secs: f64) -> Self {
        Self {
            interval: Duration::from_secs_f64(interval_secs.max(0.0)),
            last: Mutex::new(Instant::now()),
        }
    }

    pub async fn wait(&self) {
        self.wait_random(0.0, 0.0).await;
    }

    /// 在基础间隔之外再随机等待 [min, max] 秒。
    pub async fn wait_random(&self, min_secs: f64, max_secs: f64) {
        let extra = if max_secs > min_secs {
            rand::rng().random_range(min_secs..=max_secs)
        } else {
            min_secs.max(0.0)
        };
        let wait = {
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let base = (*last + self.interval).saturating_duration_since(now);
            let wait = base + Duration::from_secs_f64(extra);
            *last = now + wait;
            wait
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    /// 以当前时刻作为上一次请求的时间（用于"请求结束后再开始计时"的场景）。
    pub fn mark_now(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }
}

/// 非学习通接口（题库、大模型）使用的通用客户端。
pub fn plain_client(
    timeout: Duration,
    proxy: Option<&str>,
    accept_invalid_certs: bool,
) -> Result<Client> {
    client_builder(timeout, proxy, accept_invalid_certs, false)
}

/// 访问本机服务（例如 Ollama）的客户端：忽略系统代理。
///
/// `NO_PROXY` 中的 `*` 在部分实现里只匹配域名，不会匹配 `127.0.0.1`；
/// 代理软件也可能对代理到本机的请求返回空 502。本机接口应当始终直连。
pub fn local_client(timeout: Duration) -> Result<Client> {
    client_builder(timeout, None, false, true)
}

fn client_builder(
    timeout: Duration,
    proxy: Option<&str>,
    accept_invalid_certs: bool,
    direct: bool,
) -> Result<Client> {
    ensure_tls_provider();
    let mut builder = Client::builder()
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT)
        .danger_accept_invalid_certs(accept_invalid_certs);
    if direct {
        builder = builder.no_proxy();
    }
    if let Some(proxy) = proxy.map(str::trim).filter(|p| !p.is_empty()) {
        builder = builder
            .proxy(reqwest::Proxy::all(proxy).with_context(|| format!("代理地址无效: {proxy}"))?);
    }
    builder.build().context("创建 HTTP 客户端失败")
}

/// 截断过长的响应内容用于日志输出。
pub fn snippet(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().nth(max_chars).is_some() {
        out.push('…');
    }
    out
}

/// 判断响应是否被学习通重定向到反爬/验证码页面。
pub fn is_verification_page(url: &str, body: &str) -> bool {
    url.contains("antispiderShowVerify")
        || url.contains("processVerifyPng")
        || body.contains("antispiderShowVerify")
        || body.contains("操作异常")
        || body.contains("请输入图片中的验证码")
}

/// 检测验证页面并记录触发的 URL 与响应片段。
pub fn verification_required(url: &str, body: &str) -> Option<anyhow::Error> {
    if is_verification_page(url, body) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        if cfg!(test) {
            tracing::warn!(
                "学习通返回验证页面：{url}；响应片段：{}",
                snippet(body, 240)
            );
        } else {
            let file = format!("verification-{}-{stamp}.html", std::process::id());
            match crate::fsutil::write_private(Path::new(&file), body.as_bytes()) {
                Ok(()) => tracing::warn!(
                    "学习通返回验证页面：{url}；响应片段：{}；完整页面已保存到 {file}",
                    snippet(body, 240)
                ),
                Err(error) => tracing::warn!(
                    "学习通返回验证页面：{url}；响应片段：{}；保存页面失败：{error}",
                    snippet(body, 240)
                ),
            }
        }
        Some(crate::error::VerificationRequired.into())
    } else {
        None
    }
}

/// 读取响应体；若发现验证码页面则返回 `VerificationRequired`，避免把验证页当成空数据。
#[allow(dead_code)]
pub async fn checked_text(response: reqwest::Response) -> Result<String> {
    let status = response.status();
    let url = response.url().to_string();
    let body = response.text().await.context("读取响应内容失败")?;
    if let Some(error) = verification_required(&url, &body) {
        return Err(error);
    }
    if !status.is_success() {
        anyhow::bail!("HTTP {status}");
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("chaoxing-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cookies.txt");
        std::fs::write(&path, " _uid=123; fid=456 ;UID=123;bad;=x;vc3=a=b ").unwrap();

        let http = Http::new(&path).unwrap();
        assert_eq!(http.uid().unwrap(), "123");
        assert_eq!(http.fid(), "456");
        assert_eq!(http.cookie("vc3").as_deref(), Some("a=b"));

        let out = dir.join("out.txt");
        save_cookies(&http.store, &out).unwrap();
        let mut saved = read_cookies_file(&out);
        saved.sort();
        assert_eq!(
            saved,
            vec![
                ("UID".to_owned(), "123".to_owned()),
                ("_uid".to_owned(), "123".to_owned()),
                ("fid".to_owned(), "456".to_owned()),
                ("vc3".to_owned(), "a=b".to_owned()),
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_cookie_file_is_empty() {
        let http = Http::new("/nonexistent/cookies.txt").unwrap();
        assert!(http.uid().is_err());
        assert_eq!(http.fid(), "1024");
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limiter_spaces_calls() {
        let limiter = RateLimiter::new(2.0);
        let start = tokio::time::Instant::now();
        limiter.wait().await;
        limiter.wait().await;
        assert!(start.elapsed() >= Duration::from_secs(2));
    }

    #[test]
    fn snippet_truncates_by_chars() {
        assert_eq!(snippet("你好世界", 2), "你好…");
        assert_eq!(snippet("abc", 5), "abc");
    }

    #[test]
    fn detects_antispider_and_captcha_pages() {
        assert!(is_verification_page(
            "https://mooc1.chaoxing.com/antispiderShowVerify.ac",
            ""
        ));
        assert!(is_verification_page(
            "https://mooc1.chaoxing.com/processVerifyPng.ac?t=1",
            ""
        ));
        assert!(is_verification_page(
            "https://mooc1.chaoxing.com/mooc-ans/knowledge/cards",
            "【9010】操作异常，请输入图片中的验证码"
        ));
        // 作业页面本身也带“请输入验证码”的作业验证码输入框，不能当成反爬页。
        assert!(!is_verification_page(
            "https://mooc1.chaoxing.com/mooc-ans/work/doHomeWorkNew",
            r#"<input placeholder="请输入验证码"><img id="workVerifyImg" src="/verifyCode/stuWork?123">"#
        ));
        assert!(!is_verification_page(
            "https://mooc1.chaoxing.com/mooc-ans/knowledge/cards",
            r#"mArg = {"attachments":[]};"#
        ));
    }
}
