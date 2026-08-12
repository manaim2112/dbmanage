use crate::error::AppError;
use crate::session::{self, redirect};
use crate::state::{AppState, SessionCtx};
use crate::templates;
use axum::{
    extract::{Extension, Form, Request, State},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use sqlx::Row;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

const LOGIN_ERR: &str = "Username, password, atau kode 2FA salah";
const MAX_FAILS: u32 = 5;
const LOCK_SECS: i64 = 15 * 60;

static ATTEMPTS: LazyLock<Mutex<HashMap<String, (u32, i64)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `GET /` — buat session baru lalu redirect ke `/session_{id}/`.
pub async fn root(State(state): State<AppState>) -> Result<Response, AppError> {
    let mut bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    let sid = hex::encode(bytes);
    let ts = session::now();
    sqlx::query("INSERT INTO sessions (id, created_at, last_active, status) VALUES (?1, ?2, ?2, 'pending')")
        .bind(&sid)
        .bind(ts)
        .execute(&state.db)
        .await?;
    Ok(redirect(format!("/session_{sid}/")))
}

pub fn app_router() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/login", get(login_page).post(login_submit))
        .route("/logout", post(logout))
        .route("/setup", get(setup_page).post(setup_submit))
        .route("/totp-setup", get(totp_page).post(totp_submit))
        .merge(crate::connections::router())
        .layer(middleware::from_fn(require_session))
}

/// Route internal `/app/*` tidak boleh diakses langsung tanpa lewat `/session_*`.
async fn require_session(req: Request, next: Next) -> Response {
    if req.extensions().get::<SessionCtx>().is_none() {
        return redirect("/".to_string());
    }
    next.run(req).await
}

async fn user_count(state: &AppState) -> Result<i64, AppError> {
    let row = sqlx::query("SELECT COUNT(*) AS c FROM users")
        .fetch_one(&state.db)
        .await?;
    Ok(row.get::<i64, _>("c"))
}

async fn index(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
) -> Result<Response, AppError> {
    if user_count(&state).await? == 0 {
        return Ok(redirect(format!("/session_{}/setup", ctx.id)));
    }
    if ctx.status != "authenticated" {
        return Ok(redirect(format!("/session_{}/login", ctx.id)));
    }
    let row = sqlx::query("SELECT username FROM users WHERE id = ?")
        .bind(ctx.user_id.unwrap_or(0))
        .fetch_one(&state.db)
        .await?;
    let username: String = row.get("username");
    let connections = sqlx::query("SELECT COUNT(*) AS c FROM connections")
        .fetch_one(&state.db)
        .await?
        .get::<i64, _>("c");
    let backups = sqlx::query("SELECT COUNT(*) AS c FROM backup_configs")
        .fetch_one(&state.db)
        .await?
        .get::<i64, _>("c");
    let page = templates::Dashboard {
        username,
        connections,
        backups,
        session_short: ctx.id[..8].to_string(),
    };
    Ok(Html(templates::render(&page)?).into_response())
}

async fn login_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
) -> Result<Response, AppError> {
    if ctx.status == "authenticated" {
        return Ok(redirect(format!("/session_{}/", ctx.id)));
    }
    if user_count(&state).await? == 0 {
        return Ok(redirect(format!("/session_{}/setup", ctx.id)));
    }
    let page = templates::Login {
        error: String::new(),
    };
    Ok(Html(templates::render(&page)?).into_response())
}

#[derive(Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
    pub totp: String,
}

async fn login_submit(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    if ctx.status == "authenticated" {
        return Ok(redirect(format!("/session_{}/", ctx.id)));
    }
    if user_count(&state).await? == 0 {
        return Ok(redirect(format!("/session_{}/setup", ctx.id)));
    }
    let username = form.username.trim().to_string();
    let fail = |msg: String| -> Result<Response, AppError> {
        let page = templates::Login { error: msg };
        Ok(Html(templates::render(&page)?).into_response())
    };

    if let Some(secs) = locked_remaining_secs(&username) {
        return fail(format!(
            "Terlalu banyak percobaan gagal. Coba lagi dalam {secs} detik."
        ));
    }

    let row = sqlx::query(
        "SELECT id, password_hash, totp_secret, totp_enabled FROM users WHERE username = ?",
    )
    .bind(&username)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row else {
        record_fail(&username);
        return fail(LOGIN_ERR.into());
    };
    let id: i64 = row.get("id");
    let hash: String = row.get("password_hash");
    let totp_secret: Option<String> = row.get("totp_secret");
    let totp_enabled: i64 = row.get("totp_enabled");

    if !bcrypt::verify(&form.password, &hash).unwrap_or(false) {
        record_fail(&username);
        return fail(LOGIN_ERR.into());
    }
    if totp_enabled == 1 {
        let Some(secret) = totp_secret else {
            record_fail(&username);
            return fail(LOGIN_ERR.into());
        };
        if !crate::totp::verify(&secret, form.totp.trim())? {
            record_fail(&username);
            return fail(LOGIN_ERR.into());
        }
    }

    clear_fails(&username);
    sqlx::query("UPDATE sessions SET status = 'authenticated', user_id = ? WHERE id = ?")
        .bind(id)
        .bind(&ctx.id)
        .execute(&state.db)
        .await?;
    sqlx::query(
        "INSERT INTO audit_log (connection_id, action, detail, executed_at) VALUES (NULL, 'login', ?, ?)",
    )
    .bind(format!("login sukses: {username}"))
    .bind(session::now())
    .execute(&state.db)
    .await?;
    Ok(redirect(format!("/session_{}/", ctx.id)))
}

async fn logout(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
) -> Result<Response, AppError> {
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(&ctx.id)
        .execute(&state.db)
        .await?;
    Ok(redirect("/".to_string()))
}

async fn setup_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
) -> Result<Response, AppError> {
    if user_count(&state).await? > 0 {
        return Ok(redirect(format!("/session_{}/login", ctx.id)));
    }
    let page = templates::Setup {
        error: String::new(),
    };
    Ok(Html(templates::render(&page)?).into_response())
}

#[derive(Deserialize)]
pub struct SetupForm {
    pub username: String,
    pub password: String,
    pub password2: String,
}

async fn setup_submit(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Form(form): Form<SetupForm>,
) -> Result<Response, AppError> {
    if user_count(&state).await? > 0 {
        return Ok(redirect(format!("/session_{}/login", ctx.id)));
    }
    let fail = |msg: String| -> Result<Response, AppError> {
        let page = templates::Setup { error: msg };
        Ok(Html(templates::render(&page)?).into_response())
    };

    let username = form.username.trim().to_string();
    if username.is_empty()
        || username.len() > 32
        || !username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return fail("Username harus 1-32 karakter: huruf, angka, '-' atau '_'".into());
    }
    if form.password.len() < 12 {
        return fail("Password minimal 12 karakter".into());
    }
    if form.password != form.password2 {
        return fail("Konfirmasi password tidak sama".into());
    }

    let hash = bcrypt::hash(&form.password, 12)?;
    let secret = crate::totp::generate_secret();
    sqlx::query(
        "INSERT INTO users (username, password_hash, totp_secret, totp_enabled, created_at) VALUES (?, ?, ?, 0, ?)",
    )
    .bind(&username)
    .bind(&hash)
    .bind(&secret)
    .bind(session::now())
    .execute(&state.db)
    .await?;
    let row = sqlx::query("SELECT id FROM users WHERE username = ?")
        .bind(&username)
        .fetch_one(&state.db)
        .await?;
    let uid: i64 = row.get("id");
    sqlx::query("UPDATE sessions SET user_id = ? WHERE id = ?")
        .bind(uid)
        .bind(&ctx.id)
        .execute(&state.db)
        .await?;
    Ok(redirect(format!("/session_{}/totp-setup", ctx.id)))
}

async fn totp_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
) -> Result<Response, AppError> {
    let Some(uid) = ctx.user_id else {
        return Ok(redirect(format!("/session_{}/login", ctx.id)));
    };
    if ctx.status == "authenticated" {
        return Ok(redirect(format!("/session_{}/", ctx.id)));
    }
    let row = sqlx::query("SELECT username, totp_secret, totp_enabled FROM users WHERE id = ?")
        .bind(uid)
        .fetch_optional(&state.db)
        .await?;
    let Some(row) = row else {
        return Ok(redirect("/".to_string()));
    };
    let enabled: i64 = row.get("totp_enabled");
    if enabled == 1 {
        return Ok(redirect(format!("/session_{}/login", ctx.id)));
    }
    let username: String = row.get("username");
    let Some(secret) = row.get::<Option<String>, _>("totp_secret") else {
        return Ok(redirect("/".to_string()));
    };
    let url = crate::totp::otpauth_url(&secret, &username);
    let svg = crate::totp::qr_svg(&url);
    let page = templates::Totp {
        svg,
        secret,
        username,
        error: String::new(),
    };
    Ok(Html(templates::render(&page)?).into_response())
}

#[derive(Deserialize)]
pub struct TotpForm {
    pub code: String,
}

async fn totp_submit(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Form(form): Form<TotpForm>,
) -> Result<Response, AppError> {
    let Some(uid) = ctx.user_id else {
        return Ok(redirect(format!("/session_{}/login", ctx.id)));
    };
    let row = sqlx::query("SELECT username, totp_secret FROM users WHERE id = ?")
        .bind(uid)
        .fetch_one(&state.db)
        .await?;
    let username: String = row.get("username");
    let Some(secret) = row.get::<Option<String>, _>("totp_secret") else {
        return Ok(redirect("/".to_string()));
    };

    if crate::totp::verify(&secret, form.code.trim())? {
        sqlx::query("UPDATE users SET totp_enabled = 1 WHERE id = ?")
            .bind(uid)
            .execute(&state.db)
            .await?;
        sqlx::query("UPDATE sessions SET status = 'authenticated' WHERE id = ?")
            .bind(&ctx.id)
            .execute(&state.db)
            .await?;
        sqlx::query(
            "INSERT INTO audit_log (connection_id, action, detail, executed_at) VALUES (NULL, 'setup', ?, ?)",
        )
        .bind(format!("admin dibuat: {username}, 2FA aktif"))
        .bind(session::now())
        .execute(&state.db)
        .await?;
        Ok(redirect(format!("/session_{}/", ctx.id)))
    } else {
        let url = crate::totp::otpauth_url(&secret, &username);
        let svg = crate::totp::qr_svg(&url);
        let page = templates::Totp {
            svg,
            secret,
            username,
            error: "Kode 2FA salah atau kedaluwarsa. Coba lagi.".into(),
        };
        Ok(Html(templates::render(&page)?).into_response())
    }
}

fn locked_remaining_secs(username: &str) -> Option<i64> {
    let mut m = ATTEMPTS.lock().unwrap();
    let now = session::now();
    match m.get(username) {
        Some((fails, until)) if *fails >= MAX_FAILS && now < *until => Some(*until - now),
        Some((fails, _)) if *fails >= MAX_FAILS => {
            m.remove(username);
            None
        }
        _ => None,
    }
}

fn record_fail(username: &str) {
    let mut m = ATTEMPTS.lock().unwrap();
    let now = session::now();
    let entry = m.entry(username.to_string()).or_insert((0, 0));
    entry.0 += 1;
    if entry.0 >= MAX_FAILS {
        entry.1 = now + LOCK_SECS;
    }
}

fn clear_fails(username: &str) {
    ATTEMPTS.lock().unwrap().remove(username);
}
