use anyhow::{bail, Context, Result};
use std::path::PathBuf;

#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub data_dir: PathBuf,
    pub secret_key: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let secret_key = std::env::var("DBMANAGE_SECRET_KEY")
            .context("DBMANAGE_SECRET_KEY wajib diisi (string bebas minimal 16 karakter, dipakai sebagai key enkripsi)")?;
        if secret_key.len() < 16 {
            bail!("DBMANAGE_SECRET_KEY terlalu pendek (minimal 16 karakter)");
        }
        Ok(Self {
            host: std::env::var("DBMANAGE_HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: std::env::var("DBMANAGE_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(3000),
            data_dir: std::env::var("DBMANAGE_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("data")),
            secret_key,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("dbmanage.db")
    }
}
