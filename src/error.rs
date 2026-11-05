//! 需要用户介入、不能靠重试解决的错误。

use std::{error::Error, fmt};

/// 学习通返回了验证码/安全验证页面，自动流程无法继续。
#[derive(Debug)]
pub struct VerificationRequired;

impl fmt::Display for VerificationRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "学习通要求完成验证码/安全验证；若已开启 [captcha] enabled=true，请查看日志中的 ddddocr 错误；否则请先在浏览器打开学习通完成验证，或把 use_cookies 设为 false 重新登录后再试",
        )
    }
}

impl Error for VerificationRequired {}

/// 媒体进度上报被服务器以 HTTP 403 拒绝，可以通过刷新状态/重登/切换 rt 恢复。
#[derive(Debug)]
pub struct MediaForbidden;

impl fmt::Display for MediaForbidden {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("媒体进度上报被服务器拒绝（HTTP 403）")
    }
}

impl Error for MediaForbidden {}
