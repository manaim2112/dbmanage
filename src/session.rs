use crate::error::AppError;
use crate::state::{AppState, SessionCtx};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::Response;
use sqlx::Row;
use std::time::{SystemTime, UNIX_EPOCH};

pub const IDLE_TIMEOUT_SECS: i64 = 24 * 60 * 60;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn redirect(url: String) -> Response {
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, url)
        .body(Body::empty())
        .expect("redirect response")
}

/// Halaman yang hanya boleh diakses setelah login.
pub fn require_auth(ctx: &SessionCtx) -> Option<Response> {
    if ctx.status != "authenticated" {
        Some(redirect(format!("/session_{}/login", ctx.id)))
    } else {
        None
    }
}

/// Middleware inti: memecah URL `/session_{sid}/...`, memvalidasi session,
/// lalu menulis-ulang path internal menjadi `/app/...` agar bisa dirutekan Axum.
pub async fn session_resolver(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let path = req.uri().path().to_string();
    let Some(after) = path.strip_prefix("/session_") else {
        return Ok(next.run(req).await);
    };

    let (sid, rest) = match after.find('/') {
        Some(i) => (&after[..i], &after[i..]),
        None => (after, ""),
    };
    if sid.len() != 32 || !sid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(redirect("/".to_string()));
    }

    let row = sqlx::query("SELECT status, user_id, last_active FROM sessions WHERE id = ?")
        .bind(sid)
        .fetch_optional(&state.db)
        .await?;
    let Some(row) = row else {
        return Ok(redirect("/".to_string()));
    };
    let status: String = row.get("status");
    let user_id: Option<i64> = row.get("user_id");
    let last_active: i64 = row.get("last_active");

    if now() - last_active > IDLE_TIMEOUT_SECS {
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(sid)
            .execute(&state.db)
            .await?;
        return Ok(redirect("/".to_string()));
    }
    sqlx::query("UPDATE sessions SET last_active = ? WHERE id = ?")
        .bind(now())
        .bind(sid)
        .execute(&state.db)
        .await?;

    // `/session_{sid}` tanpa trailing slash di-redirect agar relative URL bekerja benar.
    if rest.is_empty() {
        return Ok(redirect(format!("/session_{sid}/")));
    }

    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let new_uri: Uri = format!("/app{rest}{query}").parse()?;
    *req.uri_mut() = new_uri;
    req.extensions_mut().insert(SessionCtx {
        id: sid.to_string(),
        status,
        user_id,
    });
    Ok(next.run(req).await)
}
