//! TOTP 2FA (RFC 6238): generate secret, otpauth URL, QR SVG, dan verifikasi.

use anyhow::{anyhow, Result};
use rand::RngCore;
use totp_rs::{Algorithm, TOTP};

pub const ISSUER: &str = "DBManage";

pub fn generate_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut bytes);
    base32::encode(base32::Alphabet::RFC4648 { padding: false }, &bytes)
}

pub fn otpauth_url(secret_b32: &str, username: &str) -> String {
    let account: String = username
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
        .collect();
    format!(
        "otpauth://totp/{ISSUER}:{account}?secret={secret_b32}&issuer={ISSUER}&algorithm=SHA1&digits=6&period=30"
    )
}

pub fn qr_svg(url: &str) -> String {
    let code = qrcode::QrCode::new(url.as_bytes().to_vec()).expect("data QR selalu valid");
    code.to_svg_string(4)
}

pub fn verify(secret_b32: &str, code: &str) -> Result<bool> {
    let secret = base32::decode(base32::Alphabet::RFC4648 { padding: false }, secret_b32)
        .ok_or_else(|| anyhow!("secret base32 tidak valid"))?;
    let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, secret)
        .map_err(|e| anyhow!("konfigurasi TOTP tidak valid: {e}"))?;
    match totp.check_current(code) {
        Ok(ok) => Ok(ok),
        Err(_) => Ok(false),
    }
}
