//! 登录加密与各类签名。

use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use md5::{Digest, Md5};

/// 学习通登录接口使用的 AES 密钥（同时作为 IV）
const LOGIN_AES_KEY: &[u8; 16] = b"u2oh6Vu^HWe4_AES";

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

/// AES-128-CBC + PKCS7 加密后 Base64 编码，用于登录时加密手机号与密码。
pub fn encrypt_login_field(plaintext: &str) -> String {
    let cipher = Aes128CbcEnc::new(LOGIN_AES_KEY.into(), LOGIN_AES_KEY.into());
    BASE64.encode(cipher.encrypt_padded_vec_mut::<Pkcs7>(plaintext.as_bytes()))
}

pub fn md5_hex(data: impl AsRef<[u8]>) -> String {
    format!("{:x}", Md5::digest(data.as_ref()))
}

/// 视频进度上报的 enc 签名。`playing_time` 与 `duration` 单位为秒。
pub fn video_log_enc(
    clazz_id: &str,
    uid: &str,
    jobid: &str,
    object_id: &str,
    playing_time: i64,
    duration: i64,
) -> String {
    md5_hex(format!(
        "[{clazz_id}][{uid}][{jobid}][{object_id}][{}][d_yHJ!$pdA~5][{}][0_{duration}]",
        playing_time * 1000,
        duration * 1000
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_encryption_has_expected_bytes() {
        assert_eq!(
            encrypt_login_field("13800138000"),
            "yYcVatS+4J+JGnm88UM56A=="
        );
        assert_eq!(
            encrypt_login_field("p@ssw0rd!密码"),
            "JvN6XRFteQpUVDnRFr5LvA=="
        );
        assert_eq!(
            encrypt_login_field("0123456789abcdef"),
            "hmUF/tdyZDwfeahGBesCEAAvdr+vfwefCe/qjDPK7dA="
        );
    }

    #[test]
    fn video_enc_has_expected_digest() {
        assert_eq!(
            video_log_enc("12345", "678", "1001", "abcdef", 120, 600),
            "870437a842eacaf27242f804ce791d0b"
        );
    }
}
