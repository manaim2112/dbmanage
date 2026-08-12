//! Enkripsi AES-256-GCM untuk menyimpan password koneksi database.
#![allow(dead_code)]

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use anyhow::{ensure, Result};
use rand::RngCore;
use sha2::{Digest, Sha256};

pub fn cipher_from_secret(secret: &str) -> Aes256Gcm {
    let key = Sha256::digest(secret.as_bytes());
    Aes256Gcm::new_from_slice(&key).expect("panjang key selalu valid dari SHA-256")
}

/// Hasil: hex(nonce_12_byte || ciphertext).
pub fn encrypt(cipher: &Aes256Gcm, plaintext: &str) -> Result<String> {
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("enkripsi gagal: {e:?}"))?;
    let mut out = nonce_bytes.to_vec();
    out.extend(ciphertext);
    Ok(hex::encode(out))
}

pub fn decrypt(cipher: &Aes256Gcm, data_hex: &str) -> Result<String> {
    let data = hex::decode(data_hex)?;
    ensure!(data.len() > 12, "ciphertext terlalu pendek");
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&data[..12]), &data[12..])
        .map_err(|e| anyhow::anyhow!("dekripsi gagal: {e:?}"))?;
    Ok(String::from_utf8(plaintext)?)
}
