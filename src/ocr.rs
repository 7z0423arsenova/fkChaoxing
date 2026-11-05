//! ddddocr 本地 OCR 子进程。
//!
//! 学习通的反爬验证码是简单的图片字符验证码。这里不把模型嵌进 Rust 二进制，
//! 而是启动一个常驻的 Python 子进程复用 ddddocr：模型只加载一次，之后每次
//! 识别只交换一行 JSON，避免重复加载 ONNX 带来的秒级延迟。

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;
use tracing::{debug, warn};

/// 子进程脚本在编译时嵌入，安装后不需要再依赖 resource 目录。
const WORKER_SCRIPT: &str = include_str!("../resource/ddddocr_worker.py");

/// 可替换的 OCR 后端；测试里可以注入假实现，避免依赖 Python/ddddocr。
pub trait OcrBackend: Send + Sync {
    fn recognize<'a>(
        &'a self,
        image: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
}

/// 常驻的 ddddocr 进程。
pub struct DdddOcr {
    /// 配置中显式指定的 Python 解释器；为空时依次尝试 python3/python。
    python: String,
    /// 启动和单次识别的超时时间。
    timeout: Duration,
    worker: tokio::sync::Mutex<Option<Worker>>,
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl DdddOcr {
    pub fn new(python: impl Into<String>, timeout: Duration) -> Self {
        Self {
            python: python.into(),
            timeout,
            worker: tokio::sync::Mutex::new(None),
        }
    }

    /// 识别一张验证码图片，返回去掉空白后的识别文本。
    pub async fn classify(&self, image: &[u8]) -> Result<String> {
        let mut guard = self.worker.lock().await;
        if guard.is_none() {
            *guard = Some(self.spawn().await?);
        }
        let result = match guard.as_mut() {
            Some(worker) => self.classify_with(worker, image).await,
            None => unreachable!("worker 刚刚创建"),
        };
        if result.is_err() {
            // 子进程可能已经崩溃；丢弃它，下次识别时重新启动。
            if let Some(mut worker) = guard.take() {
                let _ = worker.child.kill().await;
            }
        }
        result
    }

    async fn classify_with(&self, worker: &mut Worker, image: &[u8]) -> Result<String> {
        let request = json!({ "image": STANDARD.encode(image) });
        worker
            .stdin
            .write_all(request.to_string().as_bytes())
            .await
            .context("向 ddddocr 写入图片失败")?;
        worker
            .stdin
            .write_all(b"\n")
            .await
            .context("向 ddddocr 写入换行失败")?;
        worker
            .stdin
            .flush()
            .await
            .context("刷新 ddddocr 输入失败")?;

        let line = read_reply(&mut worker.stdout, self.timeout).await?;
        let reply: Value = serde_json::from_str(&line).context("解析 ddddocr 响应失败")?;
        if let Some(code) = reply.get("code").and_then(Value::as_str) {
            let code = normalize_code(code);
            if code.is_empty() {
                bail!("ddddocr 没有识别出字符");
            }
            return Ok(code);
        }
        if let Some(error) = reply.get("error").and_then(Value::as_str) {
            bail!("{error}");
        }
        bail!("ddddocr 返回了无法识别的响应")
    }

    async fn spawn(&self) -> Result<Worker> {
        let mut errors = Vec::new();
        for python in self.python_candidates() {
            match self.try_spawn(&python).await {
                Ok(worker) => {
                    debug!("ddddocr 已启动：{python}");
                    return Ok(worker);
                }
                Err(error) => {
                    warn!("使用 {python} 启动 ddddocr 失败：{error:#}");
                    errors.push(format!("{python}: {error:#}"));
                }
            }
        }
        bail!(
            "无法启动 ddddocr，请确认已安装 Python 和 ddddocr（python -m pip install ddddocr）；{}",
            errors.join("; ")
        )
    }

    fn python_candidates(&self) -> Vec<String> {
        let configured = self.python.trim();
        if !configured.is_empty() {
            return vec![configured.to_owned()];
        }
        vec!["python3".to_owned(), "python".to_owned()]
    }

    async fn try_spawn(&self, python: &str) -> Result<Worker> {
        let mut child = Command::new(python)
            .arg("-u")
            .arg("-c")
            .arg(WORKER_SCRIPT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("启动 {python} 失败"))?;
        let stdin = child.stdin.take().context("ddddocr 子进程没有 stdin")?;
        let stdout = child.stdout.take().context("ddddocr 子进程没有 stdout")?;
        let mut stdout = BufReader::new(stdout);

        let line = read_reply(&mut stdout, self.timeout).await?;
        let reply: Value = serde_json::from_str(&line).context("解析 ddddocr 启动响应失败")?;
        if reply.get("ready").and_then(Value::as_bool) == Some(true) {
            return Ok(Worker {
                child,
                stdin,
                stdout,
            });
        }
        if let Some(error) = reply.get("error").and_then(Value::as_str) {
            bail!("{error}");
        }
        bail!("ddddocr 返回了无法识别的启动响应")
    }
}

impl OcrBackend for DdddOcr {
    fn recognize<'a>(
        &'a self,
        image: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(self.classify(image))
    }
}

async fn read_reply(stdout: &mut BufReader<ChildStdout>, limit: Duration) -> Result<String> {
    let mut line = String::new();
    let read = timeout(limit, stdout.read_line(&mut line))
        .await
        .context("等待 ddddocr 响应超时")?
        .context("读取 ddddocr 响应失败")?;
    if read == 0 {
        bail!("ddddocr 进程已退出");
    }
    Ok(line.trim().to_owned())
}

/// 只保留验证码里可能出现的 ASCII 字母和数字。
fn normalize_code(raw: &str) -> String {
    raw.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_ocr_text() {
        assert_eq!(normalize_code(" aB3-9 \n"), "aB39");
        assert_eq!(normalize_code("你好"), "");
    }

    #[test]
    fn uses_configured_python_only() {
        let ocr = DdddOcr::new("/opt/python", Duration::from_secs(1));
        assert_eq!(ocr.python_candidates(), vec!["/opt/python".to_owned()]);
    }

    #[test]
    fn falls_back_to_common_python_names() {
        let ocr = DdddOcr::new("  ", Duration::from_secs(1));
        assert_eq!(
            ocr.python_candidates(),
            vec!["python3".to_owned(), "python".to_owned()]
        );
    }

    /// 手动验证真实 ddddocr 子进程（CI 默认跳过）：
    /// `FKCHAOXING_TEST_PYTHON=/path/to/python cargo test real_worker -- --nocapture`
    #[tokio::test]
    async fn real_worker_recognizes_captcha_when_requested() {
        let Ok(python) = std::env::var("FKCHAOXING_TEST_PYTHON") else {
            return;
        };
        if python.trim().is_empty() {
            return;
        }
        let ocr = DdddOcr::new(python, Duration::from_secs(60));
        let image = STANDARD.decode(TEST_CAPTCHA_PNG).unwrap();
        let code = ocr.classify(&image).await.unwrap();
        assert!(!code.is_empty());
    }

    /// 一张用 PIL 生成的 `a7B9` 验证码，用于手动验证真实 ddddocr 进程。
    const TEST_CAPTCHA_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAHgAAAAoCAIAAAC6iKlyAAAFxElEQVR4nO2aTUgbTRjHszHJai1+JlbioRbqB7VKW60SC7FFgrZFTC2iIh6kWBSjrQRDKagUItSEHoJae9EecmgPRUGhSrEkNiGGWINoPXjw46CgVrdRE9t82oOwDJtkdxLNvoV3f7eZ+T/PPPufze7sbpCTkxMWQ+Rh/9cF/F9gjKYJxmiaYIymCcZommCMpgnGaJqIoNEbGxtIKOj1ejx2YGAgoIbNZl+8eDEtLU0sFnd0dBiNRsoyzGZzc3Pz9evXExISUBQVCoV3795VqVQYhkXu2ANwEjHW19dDqkSn0+Gx/f39kFFlZWW/fv0KWIDNZnv8+HGwwMTERK1WG7nDJ/APGa3X6/FYeKNZLJZEIvGf3eFw5OfnkwciCPL27dvIOQDyrxidlJRkt9vx2JCMZrFYFouFMHt7eztMIIqiS0tLkTMBJ4LX6PT0dJKJFQoFKJbJZLGxscFS5eTknEb5fL7t7e2enh6CwGAwgE0Mw969ewf2NDc3r6+vHx0d6XS6zMxMvN/pdPpniwgwq+FyuWZnZ9Vq9aNHj65duxYfH8/hcFAU5fP5BQUFTU1NU1NTIS2vzWaLi4vDa4iJifn58ycoIJzRuNE4eXl5oODly5fg6MePH8HRGzdu+Hw+fJRwC+VwONvb2yHVHwZQRqvVasoFE4vFBLNI6O3tBWNlMhlBEKrRGo0GHFUqleBoa2srOOrxeLhcLij49OkTZOVhc26Xjm/fvlVVVcEo//z5o9Fo8GZUVJRcLoefaHd3V6lULi4u4j0Igty/fx/UOBwO+IQsFstqtYakDwMOjCg6OrqsrKyiouL27dsCgeDSpUs8Hm9vb89sNsvl8rW1tVOZwWD4+vVraWkpebbh4eHd3V28WVNTk56eTh6yvLyMIEiwUYVCkZGRAfYIBAKwaTKZwKbFYnG73WDPzs4OeQHnwBl/EWNjY2C2Fy9ekOvdbveVK1fAkIWFBX8Z5K6Dw+EMDQ35h8/OzhKUMplsY2PDbrfPzMxkZ2cTRqurq8/oAyWwRttstsHBQalUmpWVdXozDHjk9fX15Hm0Wi2oLy8vDyiD396lpqa+f/+eEO7z+QgXcXIaGhogfQgbKKMnJiYSEhJgKpZKpSR5fD5fbm4uqAcfUkBC3UePjIwQMphMJhRFIcOfPXsGZ1f4UBu9srICX3FlZSVJqvHxcVBcVFQUTEmy6zg8PLRYLA8fPgQFKSkpDoeDkOTLly/Jycn+RSIIcvPmTbAn4PXnfKE2+vnz52BNWVlZk5OT+/v7p6M6nQ7e6Dt37oDi0dHRYErK7d3x8TFh+Y1Go38eDMN6e3tFIlFiYiKXyxUKhbW1tWazubGxEYz98eMHpQ9nhNroe/fugTVNT0+Do4QHMBKjZ2ZmCAvm9XqDiWGM5vF4oObDhw+Ux3LK9PQ0eI/Jzc2FDDwL1Ptowk4IPI9cLhf8xfT169dgs7Ozk80OZxdvt9u/f/9eXV3tcrnA/qSkJLBptVpFIlFfX5/ZbN7a2vJ4PAcHByaTqb29/cGDBx6PB1cSXgZECsqlePr0KagvKipaXFx0OBxzc3NisZiQLdgZvbCwAMqEQqHT6SSZNNSbYXR09OHhIZhhbm4OJrCioiKU8zJ8qI2en58neVh48uQJjNG1tbWgTKVSkU8aqtGvXr0iZIAxuri4GMMwOKPOCtT2bmBgwP9njiBId3c34bVZQKNXV1ejoqJwTXx8/MHBAfmM8EbzeLyuri7wndEp5EZzudy2trbfv3/DuXQOQD2Ct7a23rp1682bN0ajEcMwPp9fWFjY1tZWWloK8zFJpVJ5vV682dLSAr66CxU2m33hwgU+n5+dnV1SUlJXV3f58mV/WX5+vsVi+fz5s16v39zc3NnZcbvdycnJV69elUgk9fX1lM/95wtywvz3jhaYr+A0wRhNE4zRNMEYTROM0TTBGE0TjNE0wRhNE4zRNMEYTROM0TTxF+8qwtl+JUl6AAAAAElFTkSuQmCC";
}
