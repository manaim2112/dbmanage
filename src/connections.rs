//! M2 — Manajemen koneksi MariaDB & PostgreSQL: CRUD, test, status online/offline.

use crate::crypto;
use crate::error::AppError;
use crate::pools::{self, PoolManager};
use crate::session::{self, redirect, require_auth};
use crate::state::{AppState, SessionCtx};
use crate::templates;
use axum::extract::{Extension, Form, Path, Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use sqlx::Row;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/connections", get(list))
        .route("/connections", post(create))
        .route("/connections/new", get(create_form))
        .route("/connections/{id}/edit", get(edit_form))
        .route("/connections/{id}", post(update))
        .route("/connections/{id}/test", post(test))
        .route("/connections/{id}/delete", post(delete))
}

#[derive(Deserialize)]
pub struct Flash {
    pub msg: Option<String>,
    pub err: Option<String>,
}

pub struct ConnRow {
    pub id: i64,
    pub name: String,
    pub db_type: String,
    pub host: String,
    pub port: i64,
    pub username: String,
    pub online: bool,
    pub version: String,
    pub latency_ms: u128,
}

#[derive(Clone)]
struct Stored {
    id: i64,
    name: String,
    db_type: String,
    host: String,
    port: i64,
    username: String,
    password: String,
}

async fn list(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Query(flash): Query<Flash>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let rows = sqlx::query(
        "SELECT id, name, db_type, host, port, username, password_encrypted
         FROM connections ORDER BY name COLLATE NOCASE",
    )
    .fetch_all(&state.db)
    .await?;

    let mut stored = Vec::new();
    for r in rows {
        let enc: String = r.get("password_encrypted");
        stored.push(Stored {
            id: r.get("id"),
            name: r.get("name"),
            db_type: r.get("db_type"),
            host: r.get("host"),
            port: r.get("port"),
            username: r.get("username"),
            password: crypto::decrypt(&state.cipher, &enc).unwrap_or_default(),
        });
    }

    // Ping semua koneksi secara paralel.
    let checks = futures::future::join_all(stored.into_iter().map(|s| {
        let pools = state.pools.clone();
        async move {
            let mut row = ConnRow {
                id: s.id,
                name: s.name.clone(),
                db_type: s.db_type.clone(),
                host: s.host.clone(),
                port: s.port,
                username: s.username.clone(),
                online: false,
                version: String::new(),
                latency_ms: 0,
            };
            let handle = match pools.get(s.id).await {
                Some(h) => h,
                None => {
                    match pools::create_pool(
                        &s.db_type,
                        &s.host,
                        s.port as u16,
                        &s.username,
                        &s.password,
                    )
                    .await
                    {
                        Ok(h) => {
                            pools.insert(s.id, h.clone()).await;
                            h
                        }
                        Err(_) => return row,
                    }
                }
            };
            let st = handle.ping().await;
            row.online = st.online;
            row.version = st.version;
            row.latency_ms = st.latency_ms;
            row
        }
    }))
    .await;

    let page = templates::Connections {
        rows: checks,
        flash_ok: flash.msg,
        flash_err: flash.err,
    };
    Ok(Html(templates::render(&page)?).into_response())
}

#[derive(Deserialize)]
pub struct ConnForm {
    pub name: String,
    pub db_type: String,
    pub host: String,
    pub port: String,
    pub username: String,
    pub password: String,
}

fn validate_common(
    name: &str,
    db_type: &str,
    host: &str,
    port: &str,
    username: &str,
) -> Result<u16, String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err("Nama koneksi wajib diisi (maks 64 karakter)".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
    {
        return Err("Nama koneksi hanya boleh huruf, angka, spasi, '-', '_', '.'".into());
    }
    if db_type != "mariadb" && db_type != "postgresql" {
        return Err("Tipe database tidak valid".into());
    }
    if host.trim().is_empty() {
        return Err("Host wajib diisi".into());
    }
    let port: u16 = port
        .trim()
        .parse()
        .map_err(|_| "Port tidak valid (harus angka)".to_string())?;
    if username.trim().is_empty() {
        return Err("Username wajib diisi".into());
    }
    Ok(port)
}

fn render_create_form(form: &ConnForm, error: &str) -> Result<Response, AppError> {
    let page = templates::ConnectionForm {
        is_edit: false,
        error: error.to_string(),
        name: form.name.clone(),
        db_type: form.db_type.clone(),
        host: form.host.clone(),
        port: form.port.clone(),
        username: form.username.clone(),
        back: ".".to_string(),
    };
    Ok(Html(templates::render(&page)?).into_response())
}

fn render_edit_form(form: &ConnForm, error: &str) -> Result<Response, AppError> {
    let page = templates::ConnectionForm {
        is_edit: true,
        error: error.to_string(),
        name: form.name.clone(),
        db_type: form.db_type.clone(),
        host: form.host.clone(),
        port: form.port.clone(),
        username: form.username.clone(),
        back: "..".to_string(),
    };
    Ok(Html(templates::render(&page)?).into_response())
}

async fn create_form(
    Extension(ctx): Extension<SessionCtx>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let page = templates::ConnectionForm {
        is_edit: false,
        error: String::new(),
        name: String::new(),
        db_type: "mariadb".to_string(),
        host: String::new(),
        port: "3306".to_string(),
        username: String::new(),
        back: ".".to_string(),
    };
    Ok(Html(templates::render(&page)?).into_response())
}

async fn create(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Form(form): Form<ConnForm>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let port = match validate_common(&form.name, &form.db_type, &form.host, &form.port, &form.username) {
        Ok(p) => p,
        Err(msg) => return render_create_form(&form, &msg),
    };
    if form.password.is_empty() {
        return render_create_form(&form, "Password wajib diisi");
    }
    let enc = crypto::encrypt(&state.cipher, &form.password)?;
    let ts = session::now();
    sqlx::query(
        "INSERT INTO connections (name, db_type, host, port, username, password_encrypted, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(form.name.trim())
    .bind(&form.db_type)
    .bind(form.host.trim())
    .bind(port as i64)
    .bind(form.username.trim())
    .bind(&enc)
    .bind(ts)
    .bind(ts)
    .execute(&state.db)
    .await?;
    sqlx::query(
        "INSERT INTO audit_log (connection_id, action, detail, executed_at) VALUES (NULL, 'connection_add', ?, ?)",
    )
    .bind(format!("koneksi ditambahkan: {}", form.name.trim()))
    .bind(ts)
    .execute(&state.db)
    .await?;
    let msg = urlenc(&format!("Koneksi '{}' ditambahkan", form.name.trim()));
    Ok(redirect(format!("/session_{}/connections?msg={msg}", ctx.id)))
}

async fn edit_form(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let row = sqlx::query("SELECT name, db_type, host, port, username FROM connections WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    let Some(row) = row else {
        return Ok(redirect(format!(
            "/session_{}/connections?err={}",
            ctx.id,
            urlenc("Koneksi tidak ditemukan")
        )));
    };
    let page = templates::ConnectionForm {
        is_edit: true,
        error: String::new(),
        name: row.get("name"),
        db_type: row.get("db_type"),
        host: row.get("host"),
        port: row.get::<i64, _>("port").to_string(),
        username: row.get("username"),
        back: "..".to_string(),
    };
    Ok(Html(templates::render(&page)?).into_response())
}

async fn update(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
    Form(form): Form<ConnForm>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let row = sqlx::query("SELECT password_encrypted FROM connections WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    let Some(row) = row else {
        return Ok(redirect(format!(
            "/session_{}/connections?err={}",
            ctx.id,
            urlenc("Koneksi tidak ditemukan")
        )));
    };
    let port = match validate_common(&form.name, &form.db_type, &form.host, &form.port, &form.username) {
        Ok(p) => p,
        Err(msg) => return render_edit_form(&form, &msg),
    };
    let enc = if form.password.is_empty() {
        row.get::<String, _>("password_encrypted")
    } else {
        crypto::encrypt(&state.cipher, &form.password)?
    };
    sqlx::query(
        "UPDATE connections SET name = ?, db_type = ?, host = ?, port = ?, username = ?, password_encrypted = ?, updated_at = ? WHERE id = ?",
    )
    .bind(form.name.trim())
    .bind(&form.db_type)
    .bind(form.host.trim())
    .bind(port as i64)
    .bind(form.username.trim())
    .bind(&enc)
    .bind(session::now())
    .bind(id)
    .execute(&state.db)
    .await?;
    // Kredensial/host bisa berubah — buang pool lama agar dibuat ulang.
    state.pools.remove(id).await;
    sqlx::query(
        "INSERT INTO audit_log (connection_id, action, detail, executed_at) VALUES (?, 'connection_update', ?, ?)",
    )
    .bind(id)
    .bind(format!("koneksi diubah: {}", form.name.trim()))
    .bind(session::now())
    .execute(&state.db)
    .await?;
    let msg = urlenc(&format!("Koneksi '{}' disimpan", form.name.trim()));
    Ok(redirect(format!("/session_{}/connections?msg={msg}", ctx.id)))
}

async fn test(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let row = sqlx::query(
        "SELECT name, db_type, host, port, username, password_encrypted FROM connections WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row else {
        return Ok(redirect(format!(
            "/session_{}/connections?err={}",
            ctx.id,
            urlenc("Koneksi tidak ditemukan")
        )));
    };
    let name: String = row.get("name");
    let db_type: String = row.get("db_type");
    let host: String = row.get("host");
    let port: i64 = row.get("port");
    let username: String = row.get("username");
    let enc: String = row.get("password_encrypted");
    let password = crypto::decrypt(&state.cipher, &enc).unwrap_or_default();

    match pools::create_pool(&db_type, &host, port as u16, &username, &password).await {
        Ok(handle) => {
            let st = handle.ping().await;
            state.pools.insert(id, handle).await;
            if st.online {
                let msg = urlenc(&format!(
                    "Koneksi '{name}' OK — {} ({} ms)",
                    st.version, st.latency_ms
                ));
                Ok(redirect(format!("/session_{}/connections?msg={msg}", ctx.id)))
            } else {
                let err = urlenc(&format!("Koneksi '{name}' terbuka tapi ping gagal"));
                Ok(redirect(format!("/session_{}/connections?err={err}", ctx.id)))
            }
        }
        Err(e) => {
            state.pools.remove(id).await;
            let err = urlenc(&format!("Gagal terhubung ke '{name}': {e}"));
            Ok(redirect(format!("/session_{}/connections?err={err}", ctx.id)))
        }
    }
}

async fn delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let row = sqlx::query("SELECT name FROM connections WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    let Some(row) = row else {
        return Ok(redirect(format!(
            "/session_{}/connections?err={}",
            ctx.id,
            urlenc("Koneksi tidak ditemukan")
        )));
    };
    let name: String = row.get("name");
    sqlx::query("DELETE FROM connections WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    state.pools.remove(id).await;
    sqlx::query(
        "INSERT INTO audit_log (connection_id, action, detail, executed_at) VALUES (NULL, 'connection_delete', ?, ?)",
    )
    .bind(format!("koneksi dihapus: {name}"))
    .bind(session::now())
    .execute(&state.db)
    .await?;
    let msg = urlenc(&format!("Koneksi '{name}' dihapus"));
    Ok(redirect(format!("/session_{}/connections?msg={msg}", ctx.id)))
}

fn urlenc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
