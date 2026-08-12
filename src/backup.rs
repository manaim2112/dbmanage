//! M4 — Autobackup ke S3 & Google Drive: konfigurasi terenkripsi,
//! dump via mysqldump/pg_dump + gzip, upload, retensi, dan scheduler.

use crate::crypto;
use crate::error::AppError;
use crate::session::{self, redirect, require_auth, urlenc};
use crate::state::{AppState, SessionCtx};
use crate::templates;
use anyhow::{bail, Context, Result};
use axum::extract::{Extension, Form, Path, Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::io::Write;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/backups", get(list))
        .route("/backups", post(create))
        .route("/backups/new", get(new_form))
        .route("/backups/{id}/edit", get(edit_form))
        .route("/backups/{id}", post(update))
        .route("/backups/{id}/delete", post(delete))
        .route("/backups/{id}/run", post(run_now))
}

// ------------------------------------------------------------ konfigurasi

#[derive(Serialize, Deserialize)]
pub struct S3Cfg {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub prefix: String,
}

#[derive(Serialize, Deserialize)]
pub struct GDriveCfg {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    pub folder_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct Schedule {
    pub freq: String, // daily | weekly
    pub time: String, // HH:MM
    pub day: String,  // mon..sun (weekly)
}

fn schedule_desc(s: &Schedule) -> String {
    match s.freq.as_str() {
        "weekly" => format!("Mingguan ({}), {}", s.day.to_uppercase(), s.time),
        _ => format!("Harian, {}", s.time),
    }
}

fn human_size(b: i64) -> String {
    let mut v = b as f64;
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", units[i])
    }
}

fn fmt_ts(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ts.to_string())
}

// ------------------------------------------------------------------ list

pub struct BackupRow {
    pub id: i64,
    pub conn_name: String,
    pub database_name: String,
    pub provider: String,
    pub schedule: String,
    pub retention_days: i64,
    pub enabled: bool,
    pub last_status: String,
    pub last_time: String,
}

pub struct HistoryRow {
    pub filename: String,
    pub status: String,
    pub provider: String,
    pub size: String,
    pub started: String,
    pub error: String,
}

async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Query(flash): Query<crate::connections::Flash>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let rows = sqlx::query(
        "SELECT b.id, COALESCE(c.name, '(koneksi terhapus)') AS conn_name, b.database_name,
                b.provider, b.schedule_cron, b.retention_days, b.enabled
         FROM backup_configs b LEFT JOIN connections c ON c.id = b.connection_id
         ORDER BY b.id",
    )
    .fetch_all(&state.db)
    .await?;

    let mut configs = Vec::new();
    for r in rows {
        let id: i64 = r.get("id");
        let sched_raw: String = r.get("schedule_cron");
        let sched: Schedule = serde_json::from_str(&sched_raw)
            .unwrap_or(Schedule { freq: "daily".into(), time: "02:00".into(), day: "sun".into() });
        let last = sqlx::query(
            "SELECT status, started_at FROM backup_history WHERE backup_config_id = ? ORDER BY started_at DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
        let (last_status, last_time) = match last {
            Some(l) => (
                l.get::<String, _>("status"),
                fmt_ts(l.get::<i64, _>("started_at")),
            ),
            None => ("belum".to_string(), String::from("—")),
        };
        configs.push(BackupRow {
            id,
            conn_name: r.get("conn_name"),
            database_name: r.get("database_name"),
            provider: r.get("provider"),
            schedule: schedule_desc(&sched),
            retention_days: r.get("retention_days"),
            enabled: r.get::<i64, _>("enabled") == 1,
            last_status,
            last_time,
        });
    }

    let hrows = sqlx::query(
        "SELECT filename, status, provider, size_bytes, started_at, error_message
         FROM backup_history ORDER BY started_at DESC LIMIT 20",
    )
    .fetch_all(&state.db)
    .await?;
    let mut history = Vec::new();
    for r in hrows {
        history.push(HistoryRow {
            filename: r.get("filename"),
            status: r.get("status"),
            provider: r.get("provider"),
            size: r
                .get::<Option<i64>, _>("size_bytes")
                .map(human_size)
                .unwrap_or_else(|| "—".to_string()),
            started: fmt_ts(r.get("started_at")),
            error: r.get::<Option<String>, _>("error_message").unwrap_or_default(),
        });
    }

    let page = templates::Backups {
        configs,
        history,
        flash_ok: flash.msg,
        flash_err: flash.err,
    };
    Ok(Html(templates::render(&page)?).into_response())
}

// ------------------------------------------------------------------ form

#[derive(Deserialize)]
pub struct BackupForm {
    pub connection_id: String,
    pub database_name: String,
    pub provider: String,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub prefix: String,
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    pub folder_id: String,
    pub freq: String,
    pub time: String,
    pub day: String,
    pub retention_days: String,
    pub enabled: Option<String>,
}

async fn load_connections(state: &AppState) -> Result<Vec<(i64, String)>, AppError> {
    let rows = sqlx::query("SELECT id, name FROM connections ORDER BY name COLLATE NOCASE")
        .fetch_all(&state.db)
        .await?;
    Ok(rows.into_iter().map(|r| (r.get("id"), r.get("name"))).collect())
}

async fn new_form(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let page = templates::BackupFormPage {
        is_edit: false,
        error: String::new(),
        connections: load_connections(&state).await?,
        conn_id: 0,
        database_name: String::new(),
        provider: "s3".to_string(),
        endpoint: String::new(),
        region: "us-east-1".to_string(),
        bucket: String::new(),
        access_key: String::new(),
        secret_key: String::new(),
        prefix: "backups/".to_string(),
        client_id: String::new(),
        client_secret: String::new(),
        refresh_token: String::new(),
        folder_id: String::new(),
        freq: "daily".to_string(),
        time: "02:00".to_string(),
        day: "sun".to_string(),
        retention_days: "30".to_string(),
        enabled: true,
    };
    Ok(Html(templates::render(&page)?).into_response())
}

struct LoadedCfg {
    conn_id: i64,
    database_name: String,
    provider: String,
    config_json: String,
    schedule_cron: String,
    retention_days: i64,
    enabled: bool,
}

fn build_loaded(form: &BackupForm) -> Result<LoadedCfg, String> {
    let conn_id: i64 = form
        .connection_id
        .trim()
        .parse()
        .map_err(|_| "Pilih koneksi yang valid".to_string())?;
    let db = form.database_name.trim();
    if db.is_empty() {
        return Err("Nama database wajib diisi".into());
    }
    let config_json = match form.provider.as_str() {
        "s3" => {
            if form.bucket.trim().is_empty() || form.access_key.trim().is_empty() || form.secret_key.trim().is_empty() {
                return Err("S3: bucket, access key, dan secret key wajib diisi".into());
            }
            serde_json::to_string(&S3Cfg {
                endpoint: form.endpoint.trim().to_string(),
                region: form.region.trim().to_string(),
                bucket: form.bucket.trim().to_string(),
                access_key: form.access_key.trim().to_string(),
                secret_key: form.secret_key.trim().to_string(),
                prefix: form.prefix.trim().to_string(),
            })
            .map_err(|e| e.to_string())?
        }
        "gdrive" => {
            if form.client_id.trim().is_empty()
                || form.client_secret.trim().is_empty()
                || form.refresh_token.trim().is_empty()
            {
                return Err("Google Drive: client ID, client secret, dan refresh token wajib diisi".into());
            }
            serde_json::to_string(&GDriveCfg {
                client_id: form.client_id.trim().to_string(),
                client_secret: form.client_secret.trim().to_string(),
                refresh_token: form.refresh_token.trim().to_string(),
                folder_id: form.folder_id.trim().to_string(),
            })
            .map_err(|e| e.to_string())?
        }
        _ => return Err("Provider tidak valid".into()),
    };
    if form.time.trim().len() != 5 {
        return Err("Jam backup harus format HH:MM".into());
    }
    let retention: i64 = form
        .retention_days
        .trim()
        .parse()
        .map_err(|_| "Retensi harus angka (hari)".to_string())?;
    if !(1..=365).contains(&retention) {
        return Err("Retensi harus 1-365 hari".into());
    }
    Ok(LoadedCfg {
        conn_id,
        database_name: db.to_string(),
        provider: form.provider.clone(),
        config_json,
        schedule_cron: serde_json::to_string(&Schedule {
            freq: form.freq.clone(),
            time: form.time.trim().to_string(),
            day: form.day.clone(),
        })
        .unwrap_or_default(),
        retention_days: retention,
        enabled: form.enabled.as_deref() == Some("1"),
    })
}

async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Form(form): Form<BackupForm>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let loaded = match build_loaded(&form) {
        Ok(l) => l,
        Err(msg) => {
            return Ok(redirect(format!(
                "/session_{}/backups/new?err={}",
                ctx.id,
                urlenc(&msg)
            )));
        }
    };
    let enc = crypto::encrypt(&state.cipher, &loaded.config_json)?;
    sqlx::query(
        "INSERT INTO backup_configs (connection_id, database_name, provider, config_json, schedule_cron, retention_days, enabled, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(loaded.conn_id)
    .bind(&loaded.database_name)
    .bind(&loaded.provider)
    .bind(&enc)
    .bind(&loaded.schedule_cron)
    .bind(loaded.retention_days)
    .bind(loaded.enabled as i64)
    .bind(session::now())
    .execute(&state.db)
    .await?;
    sqlx::query(
        "INSERT INTO audit_log (connection_id, action, detail, executed_at) VALUES (?, 'backup_config_add', ?, ?)",
    )
    .bind(loaded.conn_id)
    .bind(format!(
        "konfigurasi backup {} untuk database {}",
        loaded.provider, loaded.database_name
    ))
    .bind(session::now())
    .execute(&state.db)
    .await?;
    let msg = urlenc(&format!(
        "Konfigurasi backup untuk '{}' disimpan",
        loaded.database_name
    ));
    Ok(redirect(format!("/session_{}/backups?msg={msg}", ctx.id)))
}

#[derive(Deserialize)]
pub struct ErrFlash {
    pub err: Option<String>,
}

async fn edit_form(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
    Query(flash): Query<ErrFlash>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let row = sqlx::query(
        "SELECT connection_id, database_name, provider, config_json, schedule_cron, retention_days, enabled
         FROM backup_configs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row else {
        return Ok(redirect(format!(
            "/session_{}/backups?err={}",
            ctx.id,
            urlenc("Konfigurasi backup tidak ditemukan")
        )));
    };
    let provider: String = row.get("provider");
    let json = crypto::decrypt(&state.cipher, &row.get::<String, _>("config_json")).unwrap_or_default();
    let sched: Schedule = serde_json::from_str(&row.get::<String, _>("schedule_cron"))
        .unwrap_or(Schedule { freq: "daily".into(), time: "02:00".into(), day: "sun".into() });

    let (mut s3, mut gd) = (
        S3Cfg {
            endpoint: String::new(),
            region: "us-east-1".into(),
            bucket: String::new(),
            access_key: String::new(),
            secret_key: String::new(),
            prefix: "backups/".into(),
        },
        GDriveCfg {
            client_id: String::new(),
            client_secret: String::new(),
            refresh_token: String::new(),
            folder_id: String::new(),
        },
    );
    if provider == "s3" {
        if let Ok(v) = serde_json::from_str::<S3Cfg>(&json) {
            s3 = v;
        }
    } else if let Ok(v) = serde_json::from_str::<GDriveCfg>(&json) {
        gd = v;
    }

    let page = templates::BackupFormPage {
        is_edit: true,
        error: flash.err.unwrap_or_default(),
        connections: load_connections(&state).await?,
        conn_id: row.get("connection_id"),
        database_name: row.get("database_name"),
        provider,
        endpoint: s3.endpoint,
        region: s3.region,
        bucket: s3.bucket,
        access_key: s3.access_key,
        secret_key: s3.secret_key,
        prefix: s3.prefix,
        client_id: gd.client_id,
        client_secret: gd.client_secret,
        refresh_token: gd.refresh_token,
        folder_id: gd.folder_id,
        freq: sched.freq,
        time: sched.time,
        day: sched.day,
        retention_days: row.get::<i64, _>("retention_days").to_string(),
        enabled: row.get::<i64, _>("enabled") == 1,
    };
    Ok(Html(templates::render(&page)?).into_response())
}

async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
    Form(form): Form<BackupForm>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let loaded = match build_loaded(&form) {
        Ok(l) => l,
        Err(msg) => {
            return Ok(redirect(format!(
                "/session_{}/backups/{id}/edit?err={}",
                ctx.id,
                urlenc(&msg)
            )));
        }
    };
    let enc = crypto::encrypt(&state.cipher, &loaded.config_json)?;
    sqlx::query(
        "UPDATE backup_configs SET connection_id = ?, database_name = ?, provider = ?, config_json = ?,
         schedule_cron = ?, retention_days = ?, enabled = ? WHERE id = ?",
    )
    .bind(loaded.conn_id)
    .bind(&loaded.database_name)
    .bind(&loaded.provider)
    .bind(&enc)
    .bind(&loaded.schedule_cron)
    .bind(loaded.retention_days)
    .bind(loaded.enabled as i64)
    .bind(id)
    .execute(&state.db)
    .await?;
    let msg = urlenc("Konfigurasi backup disimpan");
    Ok(redirect(format!("/session_{}/backups?msg={msg}", ctx.id)))
}

async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    sqlx::query("DELETE FROM backup_configs WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    sqlx::query(
        "INSERT INTO audit_log (connection_id, action, detail, executed_at) VALUES (NULL, 'backup_config_delete', ?, ?)",
    )
    .bind(format!("konfigurasi backup #{id} dihapus"))
    .bind(session::now())
    .execute(&state.db)
    .await?;
    let msg = urlenc("Konfigurasi backup dihapus");
    Ok(redirect(format!("/session_{}/backups?msg={msg}", ctx.id)))
}

async fn run_now(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let exists = sqlx::query("SELECT id FROM backup_configs WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        let err = urlenc("Konfigurasi backup tidak ditemukan");
        return Ok(redirect(format!("/session_{}/backups?err={err}", ctx.id)));
    }
    let s = state.clone();
    tokio::spawn(async move {
        if let Err(e) = run_backup(s, id).await {
            tracing::error!("backup manual #{id} gagal: {e}");
        }
    });
    let msg = urlenc("Backup dimulai — pantau statusnya di riwayat");
    Ok(redirect(format!("/session_{}/backups?msg={msg}", ctx.id)))
}

// ----------------------------------------------------------- engine dump

struct BackupTarget {
    config_id: i64,
    provider: String,
    database_name: String,
    retention_days: i64,
    cfg_json: String,
    db_type: String,
    host: String,
    port: i64,
    username: String,
    password: String,
}

async fn load_target(state: &AppState, config_id: i64) -> Result<BackupTarget> {
    let row = sqlx::query(
        "SELECT b.id, b.provider, b.database_name, b.retention_days, b.config_json,
                c.db_type, c.host, c.port, c.username, c.password_encrypted
         FROM backup_configs b JOIN connections c ON c.id = b.connection_id
         WHERE b.id = ?",
    )
    .bind(config_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(r) = row else {
        bail!("konfigurasi backup tidak ditemukan");
    };
    Ok(BackupTarget {
        config_id,
        provider: r.get("provider"),
        database_name: r.get("database_name"),
        retention_days: r.get("retention_days"),
        cfg_json: crypto::decrypt(&state.cipher, &r.get::<String, _>("config_json"))?,
        db_type: r.get("db_type"),
        host: r.get("host"),
        port: r.get("port"),
        username: r.get("username"),
        password: crypto::decrypt(&state.cipher, &r.get::<String, _>("password_encrypted"))?,
    })
}

async fn dump_database(state: &AppState, t: &BackupTarget) -> Result<(std::path::PathBuf, i64)> {
    let dir = state.config.data_dir.join("backups");
    tokio::fs::create_dir_all(&dir).await?;
    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let fname = format!("{}_{}.sql.gz", t.database_name, ts);
    let path = dir.join(&fname);

    let port = t.port.to_string();
    let sql_bytes: Vec<u8> = match t.db_type.as_str() {
        "mariadb" => {
            let out = tokio::process::Command::new("mysqldump")
                .env("MYSQL_PWD", &t.password)
                .args([
                    "-h", &t.host, "-P", &port, "-u", &t.username,
                    "--single-transaction", "--routines", "--triggers", &t.database_name,
                ])
                .output()
                .await
                .context("mysqldump tidak tersedia (pasang mariadb-client)")?;
            if !out.status.success() {
                bail!("mysqldump gagal: {}", String::from_utf8_lossy(&out.stderr));
            }
            out.stdout
        }
        "postgresql" => {
            let out = tokio::process::Command::new("pg_dump")
                .env("PGPASSWORD", &t.password)
                .args(["-h", &t.host, "-p", &port, "-U", &t.username, "--no-password", "-d", &t.database_name])
                .output()
                .await
                .context("pg_dump tidak tersedia (pasang postgresql-client)")?;
            if !out.status.success() {
                bail!("pg_dump gagal: {}", String::from_utf8_lossy(&out.stderr));
            }
            out.stdout
        }
        other => bail!("tipe database tidak didukung untuk backup: {other}"),
    };

    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&sql_bytes)?;
    let gz = enc.finish()?;
    let size = gz.len() as i64;
    tokio::fs::write(&path, &gz).await?;
    Ok((path, size))
}

async fn upload_s3(cfg: &S3Cfg, path: &std::path::Path) -> Result<String> {
    use aws_sdk_s3::config::{Builder, Credentials, Region};
    use aws_sdk_s3::primitives::ByteStream;

    let creds = Credentials::new(&cfg.access_key, &cfg.secret_key, None, None, "dbmanage");
    let mut b = Builder::new()
        .region(Region::new(cfg.region.clone()))
        .credentials_provider(creds)
        .force_path_style(true);
    if !cfg.endpoint.is_empty() {
        b = b.endpoint_url(&cfg.endpoint);
    }
    let client = aws_sdk_s3::Client::from_conf(b.behavior_version_latest().build());

    let fname = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("backup.sql.gz");
    let key = format!("{}{}", cfg.prefix, fname);
    let bytes = tokio::fs::read(path).await?;
    client
        .put_object()
        .bucket(&cfg.bucket)
        .key(&key)
        .body(ByteStream::from(bytes))
        .send()
        .await
        .context("upload S3 gagal")?;
    Ok(format!("s3://{}/{}", cfg.bucket, key))
}

async fn gdrive_token(cfg: &GDriveCfg) -> Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", cfg.client_id.as_str()),
            ("client_secret", cfg.client_secret.as_str()),
            ("refresh_token", cfg.refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .context("gagal menghubungi oauth2.googleapis.com")?;
    let v: serde_json::Value = resp.json().await?;
    v["access_token"]
        .as_str()
        .map(|s| s.to_string())
        .context(format!(
            "refresh token Google Drive ditolak: {}",
            v["error_description"].as_str().unwrap_or("unknown")
        ))
}

async fn upload_gdrive(cfg: &GDriveCfg, path: &std::path::Path) -> Result<String> {
    let token = gdrive_token(cfg).await?;
    let fname = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("backup.sql.gz")
        .to_string();
    let bytes = tokio::fs::read(path).await?;

    let meta = if cfg.folder_id.is_empty() {
        serde_json::json!({ "name": fname })
    } else {
        serde_json::json!({ "name": fname, "parents": [cfg.folder_id] })
    }
    .to_string();

    let boundary = "dbmanage_boundary_8f3a1c";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n").as_bytes());
    body.extend_from_slice(meta.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}\r\nContent-Type: application/gzip\r\n\r\n").as_bytes());
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let client = reqwest::Client::new();
    let resp = client
        .post("https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart")
        .bearer_auth(&token)
        .header("Content-Type", format!("multipart/related; boundary={boundary}"))
        .body(body)
        .send()
        .await
        .context("upload Google Drive gagal")?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await?;
    if !status.is_success() {
        bail!("Google Drive menolak upload: {}", v);
    }
    Ok(format!("gdrive:{}", v["id"].as_str().unwrap_or("?")))
}

async fn apply_retention(t: &BackupTarget, dir: &std::path::Path) -> Result<()> {
    let cutoff = session::now() - t.retention_days * 86400;

    // File lokal lebih tua dari retensi.
    let mut rd = tokio::fs::read_dir(dir).await?;
    let prefix = format!("{}_", t.database_name);
    while let Some(entry) = rd.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(&prefix) {
            continue;
        }
        if let Ok(meta) = entry.metadata().await {
            if let Ok(modified) = meta.modified() {
                let m = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(i64::MAX);
                if m < cutoff {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                }
            }
        }
    }

    // Objek remote lebih tua dari retensi.
    match t.provider.as_str() {
        "s3" => {
            if let Ok(cfg) = serde_json::from_str::<S3Cfg>(&t.cfg_json) {
                retention_s3(&cfg, &prefix, cutoff).await?;
            }
        }
        "gdrive" => {
            if let Ok(cfg) = serde_json::from_str::<GDriveCfg>(&t.cfg_json) {
                retention_gdrive(&cfg, &prefix, cutoff).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn retention_s3(cfg: &S3Cfg, name_prefix: &str, cutoff: i64) -> Result<()> {
    use aws_sdk_s3::config::{Builder, Credentials, Region};
    let creds = Credentials::new(&cfg.access_key, &cfg.secret_key, None, None, "dbmanage");
    let mut b = Builder::new()
        .region(Region::new(cfg.region.clone()))
        .credentials_provider(creds)
        .force_path_style(true);
    if !cfg.endpoint.is_empty() {
        b = b.endpoint_url(&cfg.endpoint);
    }
    let client = aws_sdk_s3::Client::from_conf(b.behavior_version_latest().build());
    let resp = client
        .list_objects_v2()
        .bucket(&cfg.bucket)
        .prefix(format!("{}{}", cfg.prefix, name_prefix))
        .send()
        .await?;
    if let Some(items) = resp.contents() {
        for obj in items {
            let Some(key) = obj.key() else { continue };
            let age_ok = obj
                .last_modified()
                .map(|d| d.secs() < cutoff)
                .unwrap_or(false);
            if age_ok {
                let _ = client.delete_object().bucket(&cfg.bucket).key(key).send().await;
            }
        }
    }
    Ok(())
}

async fn retention_gdrive(cfg: &GDriveCfg, name_prefix: &str, cutoff: i64) -> Result<()> {
    if cfg.folder_id.is_empty() {
        return Ok(());
    }
    let token = gdrive_token(cfg).await?;
    let client = reqwest::Client::new();
    let q = format!("name contains '{name_prefix}' and '{}' in parents", cfg.folder_id);
    let resp = client
        .get("https://www.googleapis.com/drive/v3/files")
        .bearer_auth(&token)
        .query(&[("q", q.as_str()), ("fields", "files(id,name,createdTime)"), ("pageSize", "1000")])
        .send()
        .await?;
    let v: serde_json::Value = resp.json().await?;
    for f in v["files"].as_array().cloned().unwrap_or_default() {
        let created = f["createdTime"].as_str().unwrap_or("");
        let ts = chrono::DateTime::parse_from_rfc3339(created)
            .map(|d| d.timestamp())
            .unwrap_or(i64::MAX);
        if ts < cutoff {
            if let Some(fid) = f["id"].as_str() {
                let _ = client
                    .delete(format!("https://www.googleapis.com/drive/v3/files/{fid}"))
                    .bearer_auth(&token)
                    .send()
                    .await;
            }
        }
    }
    Ok(())
}

/// Eksekusi satu backup penuh: dump → gzip → upload → retensi → riwayat.
pub async fn run_backup(state: AppState, config_id: i64) -> Result<()> {
    let target = load_target(&state, config_id).await?;
    let started = session::now();
    let hist = sqlx::query(
        "INSERT INTO backup_history (backup_config_id, filename, size_bytes, status, provider, remote_path, error_message, started_at, finished_at)
         VALUES (?, '', NULL, 'running', ?, NULL, NULL, ?, NULL)",
    )
    .bind(config_id)
    .bind(&target.provider)
    .bind(started)
    .execute(&state.db)
    .await?;
    let hist_id = hist.last_insert_rowid();

    let outcome = run_backup_inner(&state, &target).await;
    let finished = session::now();
    match &outcome {
        Ok((fname, size, remote)) => {
            sqlx::query(
                "UPDATE backup_history SET filename = ?, size_bytes = ?, status = 'success', remote_path = ?, finished_at = ? WHERE id = ?",
            )
            .bind(fname)
            .bind(size)
            .bind(remote)
            .bind(finished)
            .bind(hist_id)
            .execute(&state.db)
            .await?;
            sqlx::query(
                "INSERT INTO audit_log (connection_id, action, detail, executed_at) VALUES (NULL, 'backup_success', ?, ?)",
            )
            .bind(format!("backup {fname} → {remote}"))
            .bind(finished)
            .execute(&state.db)
            .await?;
            tracing::info!("backup selesai: {fname} → {remote} ({size} byte)");
        }
        Err(e) => {
            let msg = e.to_string();
            sqlx::query(
                "UPDATE backup_history SET status = 'failed', error_message = ?, finished_at = ? WHERE id = ?",
            )
            .bind(&msg)
            .bind(finished)
            .bind(hist_id)
            .execute(&state.db)
            .await?;
            sqlx::query(
                "INSERT INTO audit_log (connection_id, action, detail, executed_at) VALUES (NULL, 'backup_failed', ?, ?)",
            )
            .bind(format!("backup database {} gagal: {msg}", target.database_name))
            .bind(finished)
            .execute(&state.db)
            .await?;
        }
    }
    outcome.map(|_| ())
}

async fn run_backup_inner(state: &AppState, t: &BackupTarget) -> Result<(String, i64, String)> {
    let (path, size) = dump_database(state, t).await?;
    let fname = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("backup.sql.gz")
        .to_string();
    let remote = match t.provider.as_str() {
        "s3" => {
            let cfg: S3Cfg = serde_json::from_str(&t.cfg_json).context("konfigurasi S3 tidak valid")?;
            upload_s3(&cfg, &path).await?
        }
        "gdrive" => {
            let cfg: GDriveCfg =
                serde_json::from_str(&t.cfg_json).context("konfigurasi Google Drive tidak valid")?;
            upload_gdrive(&cfg, &path).await?
        }
        other => bail!("provider tidak dikenal: {other}"),
    };
    if let Some(dir) = path.parent() {
        let _ = apply_retention(t, dir).await;
    }
    Ok((fname, size, remote))
}

// ------------------------------------------------------------- scheduler

pub fn spawn_scheduler(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            ticker.tick().await;
            if let Err(e) = scheduler_tick(&state).await {
                tracing::error!("scheduler backup: {e}");
            }
        }
    });
}

async fn scheduler_tick(state: &AppState) -> Result<()> {
    let now = chrono::Local::now();
    let hhmm = now.format("%H:%M").to_string();
    let dow = now.format("%a").to_lowercase();
    let today = now.format("%Y-%m-%d").to_string();

    let rows = sqlx::query(
        "SELECT id, schedule_cron FROM backup_configs WHERE enabled = 1",
    )
    .fetch_all(&state.db)
    .await?;

    for r in rows {
        let id: i64 = r.get("id");
        let Ok(sched) = serde_json::from_str::<Schedule>(&r.get::<String, _>("schedule_cron"))
        else {
            continue;
        };
        if sched.time != hhmm {
            continue;
        }
        if sched.freq == "weekly" && sched.day.to_lowercase() != dow {
            continue;
        }
        // Jangan jalankan dua kali di hari yang sama.
        let last = sqlx::query(
            "SELECT started_at FROM backup_history WHERE backup_config_id = ? ORDER BY started_at DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
        if let Some(l) = last {
            let ran_day = fmt_ts(l.get("started_at"));
            if ran_day.starts_with(&today) {
                continue;
            }
        }
        tracing::info!("scheduler: menjalankan backup konfigurasi #{id}");
        let s = state.clone();
        tokio::spawn(async move {
            if let Err(e) = run_backup(s, id).await {
                tracing::error!("backup terjadwal #{id} gagal: {e}");
            }
        });
    }
    Ok(())
}
