//! M3 — Database Explorer: database, user, tabel, grid data (CRUD),
//! relasi/ERD, DDL, dan SQL editor untuk MariaDB & PostgreSQL.

use crate::crypto;
use crate::error::AppError;
use crate::metadata::{self, ColumnInfo, DbRef};
use crate::pools::{self, PoolHandle};
use crate::session::{self, redirect, require_auth, urlenc};
use crate::state::{AppState, SessionCtx};
use crate::templates;
use axum::extract::{Extension, Form, Path, Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use sqlx::{Column, Row, ValueRef};
use std::collections::HashMap;
use std::time::Instant;

pub const PAGE_SIZE: i64 = 50;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/connections/{id}/databases", get(databases))
        .route("/connections/{id}/databases", post(create_database))
        .route("/connections/{id}/databases/{db}/delete", post(drop_database))
        .route("/connections/{id}/users", get(users_page))
        .route("/connections/{id}/users", post(create_user))
        .route("/connections/{id}/users/delete", post(drop_user))
        .route("/connections/{id}/databases/{db}/tables", get(tables_page))
        .route("/connections/{id}/databases/{db}/erd", get(erd))
        .route("/connections/{id}/databases/{db}/query", get(query_page))
        .route("/connections/{id}/databases/{db}/query", post(query_run))
        .route("/connections/{id}/databases/{db}/tables/{tbl}", get(table_detail))
        .route(
            "/connections/{id}/databases/{db}/tables/{tbl}/rows",
            post(row_insert),
        )
        .route(
            "/connections/{id}/databases/{db}/tables/{tbl}/rows/update",
            post(row_update),
        )
        .route(
            "/connections/{id}/databases/{db}/tables/{tbl}/rows/delete",
            post(row_delete),
        )
}

// ------------------------------------------------------------- internal

struct Conn {
    id: i64,
    name: String,
    db_type: String,
    host: String,
    port: i64,
    username: String,
    password: String,
}

async fn load_conn(state: &AppState, id: i64) -> Result<Option<Conn>, AppError> {
    let row = sqlx::query(
        "SELECT id, name, db_type, host, port, username, password_encrypted
         FROM connections WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|r| Conn {
        id: r.get("id"),
        name: r.get("name"),
        db_type: r.get("db_type"),
        host: r.get("host"),
        port: r.get("port"),
        username: r.get("username"),
        password: crypto::decrypt(&state.cipher, &r.get::<String, _>("password_encrypted"))
            .unwrap_or_default(),
    }))
}

async fn pool_for(state: &AppState, conn: &Conn) -> Result<PoolHandle, AppError> {
    if let Some(h) = state.pools.get(conn.id).await {
        return Ok(h);
    }
    let h = pools::create_pool(
        &conn.db_type,
        &conn.host,
        conn.port as u16,
        &conn.username,
        &conn.password,
    )
    .await?;
    state.pools.insert(conn.id, h.clone()).await;
    Ok(h)
}

/// DbRef terikat ke satu database tertentu.
async fn dbr_for(state: &AppState, conn: &Conn, db: &str) -> Result<DbRef, AppError> {
    match conn.db_type.as_str() {
        "mariadb" => match pool_for(state, conn).await? {
            PoolHandle::MySql(p) => Ok(DbRef::MySql(p, db.to_string())),
            _ => Err(anyhow::anyhow!("tipe pool tidak cocok untuk koneksi ini").into()),
        },
        "postgresql" => {
            let p = state
                .pools
                .pg_pool(
                    conn.id,
                    db,
                    &conn.host,
                    conn.port as u16,
                    &conn.username,
                    &conn.password,
                )
                .await?;
            Ok(DbRef::Pg(p))
        }
        t => Err(anyhow::anyhow!("tipe database tidak dikenal: {t}").into()),
    }
}

fn flash_to(ctx: &SessionCtx, to: &str, ok: Option<&str>, err: Option<&str>) -> Response {
    let mut url = format!("/session_{}{}", ctx.id, to);
    let mut qs = Vec::new();
    if let Some(m) = ok {
        qs.push(format!("msg={}", urlenc(m)));
    }
    if let Some(e) = err {
        qs.push(format!("err={}", urlenc(e)));
    }
    if !qs.is_empty() {
        url.push('?');
        url.push_str(&qs.join("&"));
    }
    redirect(url)
}

async fn audit(
    state: &AppState,
    conn_id: Option<i64>,
    action: &str,
    detail: &str,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO audit_log (connection_id, action, detail, executed_at) VALUES (?, ?, ?, ?)")
        .bind(conn_id)
        .bind(action)
        .bind(detail)
        .bind(session::now())
        .execute(&state.db)
        .await?;
    Ok(())
}

fn gen_password(n: usize) -> String {
    use rand::Rng;
    const SET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!@#$%^&*-_=+";
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| SET[rng.gen_range(0..SET.len())] as char)
        .collect()
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

// ------------------------------------------------------------- database

async fn databases(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
    Query(flash): Query<crate::connections::Flash>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let handle = pool_for(&state, &conn).await?;
    let dbr = match &handle {
        PoolHandle::MySql(p) => DbRef::MySql(p.clone(), String::new()),
        PoolHandle::Postgres(p) => DbRef::Pg(p.clone()),
    };
    let dbs = metadata::list_databases(&dbr).await?;
    let page = templates::Databases {
        conn_id: conn.id,
        conn_name: conn.name,
        db_type: conn.db_type,
        dbs,
        flash_ok: flash.msg,
        flash_err: flash.err,
    };
    Ok(Html(templates::render(&page)?).into_response())
}

#[derive(Deserialize)]
pub struct NewDbForm {
    pub name: String,
}

async fn create_database(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
    Form(form): Form<NewDbForm>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let to = format!("/connections/{id}/databases");
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let name = form.name.trim().to_string();
    if let Err(e) = metadata::assert_identifier(&name, "Nama database") {
        return Ok(flash_to(&ctx, &to, None, Some(&e.to_string())));
    }
    let handle = pool_for(&state, &conn).await?;
    let dbr = match &handle {
        PoolHandle::MySql(p) => DbRef::MySql(p.clone(), String::new()),
        PoolHandle::Postgres(p) => DbRef::Pg(p.clone()),
    };
    match metadata::create_database(&dbr, &name).await {
        Ok(()) => {
            audit(&state, Some(id), "database_create", &format!("database dibuat: {name}")).await?;
            Ok(flash_to(&ctx, &to, Some(&format!("Database '{name}' dibuat")), None))
        }
        Err(e) => Ok(flash_to(&ctx, &to, None, Some(&format!("Gagal membuat database: {e}")))),
    }
}

async fn drop_database(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path((id, db)): Path<(i64, String)>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let to = format!("/connections/{id}/databases");
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    if conn.db_type == "postgresql" {
        state.pools.close_pg_db(id, &db).await;
    }
    let handle = pool_for(&state, &conn).await?;
    let dbr = match &handle {
        PoolHandle::MySql(p) => DbRef::MySql(p.clone(), String::new()),
        PoolHandle::Postgres(p) => DbRef::Pg(p.clone()),
    };
    match metadata::drop_database(&dbr, &db).await {
        Ok(()) => {
            audit(&state, Some(id), "database_drop", &format!("database dihapus: {db}")).await?;
            Ok(flash_to(&ctx, &to, Some(&format!("Database '{db}' dihapus")), None))
        }
        Err(e) => Ok(flash_to(
            &ctx,
            &to,
            None,
            Some(&format!("Gagal menghapus database '{db}': {e}")),
        )),
    }
}

// ---------------------------------------------------------------- user

async fn users_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
    Query(flash): Query<crate::connections::Flash>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let handle = pool_for(&state, &conn).await?;
    let dbr = match &handle {
        PoolHandle::MySql(p) => DbRef::MySql(p.clone(), String::new()),
        PoolHandle::Postgres(p) => DbRef::Pg(p.clone()),
    };
    let users = metadata::list_users(&dbr).await?;
    let dbs = metadata::list_databases(&dbr).await?;
    let page = templates::Users {
        conn_id: conn.id,
        conn_name: conn.name,
        db_type: conn.db_type,
        users,
        dbs,
        flash_ok: flash.msg,
        flash_err: flash.err,
    };
    Ok(Html(templates::render(&page)?).into_response())
}

#[derive(Deserialize)]
pub struct NewUserForm {
    pub username: String,
    pub password: String,
    pub grant_db: String,
    pub priv_level: String,
}

async fn create_user(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
    Form(form): Form<NewUserForm>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let to = format!("/connections/{id}/users");
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let username = form.username.trim().to_string();
    if let Err(e) = metadata::assert_identifier(&username, "Username") {
        return Ok(flash_to(&ctx, &to, None, Some(&e.to_string())));
    }
    let password = if form.password.is_empty() {
        gen_password(24)
    } else {
        form.password.clone()
    };
    let handle = pool_for(&state, &conn).await?;
    let dbr = match &handle {
        PoolHandle::MySql(p) => DbRef::MySql(p.clone(), String::new()),
        PoolHandle::Postgres(p) => DbRef::Pg(p.clone()),
    };
    match metadata::create_user(&dbr, &username, &password, &form.grant_db, &form.priv_level).await
    {
        Ok(()) => {
            audit(
                &state,
                Some(id),
                "user_create",
                &format!("user dibuat: {username} (grant: {})", form.grant_db),
            )
            .await?;
            Ok(flash_to(
                &ctx,
                &to,
                Some(&format!(
                    "User '{username}' dibuat. Password: {password} — simpan sekarang, tidak ditampilkan lagi."
                )),
                None,
            ))
        }
        Err(e) => Ok(flash_to(&ctx, &to, None, Some(&format!("Gagal membuat user: {e}")))),
    }
}

#[derive(Deserialize)]
pub struct DropUserForm {
    pub username: String,
    pub host: String,
}

async fn drop_user(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path(id): Path<i64>,
    Form(form): Form<DropUserForm>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let to = format!("/connections/{id}/users");
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let handle = pool_for(&state, &conn).await?;
    let dbr = match &handle {
        PoolHandle::MySql(p) => DbRef::MySql(p.clone(), String::new()),
        PoolHandle::Postgres(p) => DbRef::Pg(p.clone()),
    };
    match metadata::drop_user(&dbr, &form.username, &form.host).await {
        Ok(()) => {
            audit(&state, Some(id), "user_drop", &format!("user dihapus: {}", form.username)).await?;
            Ok(flash_to(&ctx, &to, Some(&format!("User '{}' dihapus", form.username)), None))
        }
        Err(e) => Ok(flash_to(
            &ctx,
            &to,
            None,
            Some(&format!("Gagal menghapus user '{}': {e}", form.username)),
        )),
    }
}

// ---------------------------------------------------------------- tabel

pub struct TableDisplay {
    pub name: String,
    pub engine: String,
    pub rows: i64,
    pub size: String,
    pub collation: String,
    pub fk_count: i64,
}

async fn tables_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path((id, db)): Path<(i64, String)>,
    Query(flash): Query<crate::connections::Flash>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let dbr = dbr_for(&state, &conn, &db).await?;
    let tables = metadata::list_tables(&dbr, &db).await?;
    let rows: Vec<TableDisplay> = tables
        .into_iter()
        .map(|t| TableDisplay {
            name: t.name,
            engine: t.engine,
            rows: t.rows,
            size: human_size(t.size),
            collation: t.collation,
            fk_count: t.fk_count,
        })
        .collect();
    let page = templates::Tables {
        conn_id: conn.id,
        conn_name: conn.name,
        db,
        tables: rows,
        flash_ok: flash.msg,
        flash_err: flash.err,
    };
    Ok(Html(templates::render(&page)?).into_response())
}

// ------------------------------------------------------------- ERD

async fn erd(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path((id, db)): Path<(i64, String)>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let dbr = dbr_for(&state, &conn, &db).await?;
    let tables = metadata::list_tables(&dbr, &db).await?;
    let fks = metadata::all_fks(&dbr, &db).await?;
    let pks = metadata::pk_map(&dbr, &db).await?;

    let mut m = String::from("erDiagram\n");
    for t in &tables {
        let idn = metadata::mermaid_ident(&t.name);
        match pks.get(&t.name) {
            Some(cols) if !cols.is_empty() => {
                m.push_str(&format!("    {idn} {{\n"));
                for c in cols {
                    m.push_str(&format!("        key {} PK\n", metadata::mermaid_ident(c)));
                }
                m.push_str("    }\n");
            }
            _ => m.push_str(&format!("    {idn}\n")),
        }
    }
    for e in &fks {
        let label = format!("{} > {}", e.column, e.ref_column).replace('"', "");
        m.push_str(&format!(
            "    {} }}o--|| {} : \"{}\"\n",
            metadata::mermaid_ident(&e.table),
            metadata::mermaid_ident(&e.ref_table),
            label
        ));
    }

    let page = templates::Erd {
        conn_id: conn.id,
        conn_name: conn.name,
        db,
        mermaid: m,
        has_fks: !fks.is_empty(),
    };
    Ok(Html(templates::render(&page)?).into_response())
}

// ------------------------------------------------------------- SQL editor

#[derive(Deserialize)]
pub struct SqlForm {
    pub sql: String,
    pub write: Option<String>,
}

async fn query_page(
    Extension(ctx): Extension<SessionCtx>,
    Path((id, db)): Path<(i64, String)>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let page = templates::Query {
        conn_id: conn.id,
        conn_name: conn.name,
        db,
        sql: String::new(),
        write: false,
        result: None,
        error: String::new(),
    };
    Ok(Html(templates::render(&page)?).into_response())
}

fn render_query(
    conn: &Conn,
    db: &str,
    sql: &str,
    write: bool,
    result: Option<templates::QueryResult>,
    error: &str,
) -> Result<Response, AppError> {
    let page = templates::Query {
        conn_id: conn.id,
        conn_name: conn.name.clone(),
        db: db.to_string(),
        sql: sql.to_string(),
        write,
        result,
        error: error.to_string(),
    };
    Ok(Html(templates::render(&page)?).into_response())
}

async fn query_run(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path((id, db)): Path<(i64, String)>,
    Form(form): Form<SqlForm>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let sql = form.sql.trim().to_string();
    let write = form.write.as_deref() == Some("1");
    if sql.is_empty() {
        return render_query(&conn, &db, &sql, write, None, "Query kosong");
    }
    let first = sql.split_whitespace().next().unwrap_or("").to_uppercase();
    let is_read = matches!(first.as_str(), "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN");
    if !is_read && !write {
        return render_query(
            &conn,
            &db,
            &sql,
            write,
            None,
            "Mode tulis nonaktif. Centang \"Mode tulis\" untuk menjalankan INSERT/UPDATE/DELETE/DDL.",
        );
    }
    let dbr = dbr_for(&state, &conn, &db).await?;
    let start = Instant::now();
    if is_read {
        let fetched: Result<(Vec<String>, Vec<Vec<String>>), anyhow::Error> = async {
            let (columns, data) = match &dbr {
                DbRef::MySql(p, _) => {
                    let rows = sqlx::query(&sql).fetch_all(p).await?;
                    let columns = rows
                        .first()
                        .map(|r| r.columns().iter().map(|c| c.name().to_string()).collect())
                        .unwrap_or_default();
                    let data = rows
                        .iter()
                        .map(|r| (0..columns.len()).map(|i| mysql_cell(r, i)).collect())
                        .collect();
                    (columns, data)
                }
                DbRef::Pg(p) => {
                    let rows = sqlx::query(&sql).fetch_all(p).await?;
                    let columns = rows
                        .first()
                        .map(|r| r.columns().iter().map(|c| c.name().to_string()).collect())
                        .unwrap_or_default();
                    let data = rows
                        .iter()
                        .map(|r| (0..columns.len()).map(|i| pg_cell(r, i)).collect())
                        .collect();
                    (columns, data)
                }
            };
            Ok((columns, data))
        }
        .await;
        return match fetched {
            Ok((columns, rows)) => Ok(render_query(
                &conn,
                &db,
                &sql,
                write,
                Some(templates::QueryResult {
                    row_count: rows.len(),
                    columns,
                    rows,
                    affected: None,
                    duration_ms: start.elapsed().as_millis(),
                }),
                "",
            )?),
            Err(e) => render_query(&conn, &db, &sql, write, None, &format!("Query gagal: {e}")),
        };
    }
    match dbr.execute(&sql).await {
        Ok(n) => {
            let short: String = sql.chars().take(200).collect();
            audit(&state, Some(id), "sql_write", &format!("[{db}] {short}")).await?;
            render_query(
                &conn,
                &db,
                &sql,
                write,
                Some(templates::QueryResult {
                    row_count: 0,
                    columns: Vec::new(),
                    rows: Vec::new(),
                    affected: Some(n),
                    duration_ms: start.elapsed().as_millis(),
                }),
                "",
            )
        }
        Err(e) => render_query(&conn, &db, &sql, write, None, &format!("Query gagal: {e}")),
    }
}

macro_rules! try_opt {
    ($row:expr, $i:expr, $t:ty, $fmt:expr) => {
        if let Ok(v) = $row.try_get::<Option<$t>, _>($i) {
            return match v {
                Some(x) => $fmt(x),
                None => "NULL".to_string(),
            };
        }
    };
}

fn cell_fallback(row_null: bool) -> String {
    if row_null {
        "NULL".to_string()
    } else {
        "(tipe tidak didukung)".to_string()
    }
}

fn mysql_cell(row: &sqlx::mysql::MySqlRow, i: usize) -> String {
    try_opt!(row, i, i64, |x: i64| x.to_string());
    try_opt!(row, i, i32, |x: i32| x.to_string());
    try_opt!(row, i, i8, |x: i8| x.to_string());
    try_opt!(row, i, f64, |x: f64| x.to_string());
    try_opt!(row, i, f32, |x: f32| x.to_string());
    try_opt!(row, i, chrono::NaiveDateTime, |x: chrono::NaiveDateTime| x
        .format("%Y-%m-%d %H:%M:%S")
        .to_string());
    try_opt!(row, i, chrono::NaiveDate, |x: chrono::NaiveDate| x
        .format("%Y-%m-%d")
        .to_string());
    try_opt!(row, i, chrono::NaiveTime, |x: chrono::NaiveTime| x
        .format("%H:%M:%S")
        .to_string());
    try_opt!(row, i, chrono::Duration, |x: chrono::Duration| format!("{}s", x.num_seconds()));
    try_opt!(row, i, String, |x: String| x);
    try_opt!(row, i, Vec<u8>, |x: Vec<u8>| format!("({} byte)", x.len()));
    try_opt!(row, i, serde_json::Value, |x: serde_json::Value| x.to_string());
    cell_fallback(row.try_get_raw(i).map(|r| r.is_null()).unwrap_or(false))
}

fn pg_cell(row: &sqlx::postgres::PgRow, i: usize) -> String {
    try_opt!(row, i, bool, |x: bool| x.to_string());
    try_opt!(row, i, i64, |x: i64| x.to_string());
    try_opt!(row, i, i32, |x: i32| x.to_string());
    try_opt!(row, i, i16, |x: i16| x.to_string());
    try_opt!(row, i, f64, |x: f64| x.to_string());
    try_opt!(row, i, f32, |x: f32| x.to_string());
    try_opt!(row, i, chrono::NaiveDateTime, |x: chrono::NaiveDateTime| x
        .format("%Y-%m-%d %H:%M:%S")
        .to_string());
    try_opt!(row, i, chrono::DateTime<chrono::Utc>, |x: chrono::DateTime<chrono::Utc>| x
        .format("%Y-%m-%d %H:%M:%S %Z")
        .to_string());
    try_opt!(row, i, chrono::NaiveDate, |x: chrono::NaiveDate| x
        .format("%Y-%m-%d")
        .to_string());
    try_opt!(row, i, chrono::NaiveTime, |x: chrono::NaiveTime| x
        .format("%H:%M:%S")
        .to_string());
    try_opt!(row, i, String, |x: String| x);
    try_opt!(row, i, Vec<u8>, |x: Vec<u8>| format!("({} byte)", x.len()));
    try_opt!(row, i, serde_json::Value, |x: serde_json::Value| x.to_string());
    try_opt!(row, i, uuid::Uuid, |x: uuid::Uuid| x.to_string());
    cell_fallback(row.try_get_raw(i).map(|r| r.is_null()).unwrap_or(false))
}

// ------------------------------------------------------------- detail tabel

#[derive(Deserialize)]
pub struct TableQuery {
    pub tab: Option<String>,
    pub page: Option<i64>,
    pub fcol: Option<String>,
    pub fop: Option<String>,
    pub fval: Option<String>,
    pub sort: Option<String>,
    pub dir: Option<String>,
    pub new: Option<String>,
    pub edit: Option<String>,
}

pub struct CellVal {
    pub v: String,
    pub null: bool,
}

pub struct GridRow {
    pub pk: String,
    pub cells: Vec<CellVal>,
}

pub struct Grid {
    pub cols: Vec<String>,
    pub categories: Vec<String>,
    pub rows: Vec<GridRow>,
    pub pk_col: String,
    pub pk_index: i64,
    pub page: i64,
    pub pages: i64,
    pub total: i64,
    pub filter_qs: String,
    pub sort_qs: String,
    pub filter_col: String,
    pub filter_op: String,
    pub filter_val: String,
    pub sort: String,
    pub dir: String,
    pub new_row: bool,
    pub edit_pk: String,
}

fn cast_expr(dialect: &str, col: &ColumnInfo) -> String {
    let q = metadata::qi(dialect, &col.name);
    if col.category == "bin" {
        return match dialect {
            "mariadb" => format!("CAST(OCTET_LENGTH({q}) AS CHAR)"),
            _ => format!("octet_length({q})::text"),
        };
    }
    match dialect {
        "mariadb" => format!("CAST({q} AS CHAR)"),
        _ => format!("{q}::text"),
    }
}

fn grid_cell_mysql(row: &sqlx::mysql::MySqlRow, i: usize) -> CellVal {
    match row.try_get::<Option<String>, _>(i) {
        Ok(v) => CellVal {
            v: v.clone().unwrap_or_default(),
            null: v.is_none(),
        },
        Err(_) => match row.try_get::<Option<Vec<u8>>, _>(i) {
            Ok(Some(b)) => CellVal {
                v: format!("({} byte)", b.len()),
                null: false,
            },
            Ok(None) => CellVal {
                v: String::new(),
                null: true,
            },
            Err(_) => CellVal {
                v: "(?)".to_string(),
                null: false,
            },
        },
    }
}

fn grid_cell_pg(row: &sqlx::postgres::PgRow, i: usize) -> CellVal {
    match row.try_get::<Option<String>, _>(i) {
        Ok(v) => CellVal {
            v: v.clone().unwrap_or_default(),
            null: v.is_none(),
        },
        Err(_) => match row.try_get::<Option<Vec<u8>>, _>(i) {
            Ok(Some(b)) => CellVal {
                v: format!("({} byte)", b.len()),
                null: false,
            },
            Ok(None) => CellVal {
                v: String::new(),
                null: true,
            },
            Err(_) => CellVal {
                v: "(?)".to_string(),
                null: false,
            },
        },
    }
}

async fn build_grid(
    dbr: &DbRef,
    tbl: &str,
    columns: &[ColumnInfo],
    pk: &[String],
    q: &TableQuery,
) -> Result<Grid, AppError> {
    let dialect = dbr.dialect();
    let names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();

    let fcol = q.fcol.clone().unwrap_or_default();
    let fop = q.fop.clone().unwrap_or_else(|| "=".to_string());
    let fval = q.fval.clone().unwrap_or_default();
    let sort = q.sort.clone().unwrap_or_default();
    let dir = if q.dir.as_deref() == Some("desc") { "desc" } else { "asc" };
    let page = q.page.unwrap_or(0).max(0);

    let op_ok = matches!(fop.as_str(), "=" | "!=" | "LIKE" | ">" | ">=" | "<" | "<=");
    let mut where_sql = String::new();
    if !fval.is_empty() && names.contains(&fcol.as_str()) && op_ok {
        let col = columns.iter().find(|c| c.name == fcol).unwrap();
        let expr = metadata::qi(dialect, &col.name);
        let val = if fop == "LIKE" {
            format!("%{fval}%")
        } else {
            fval.clone()
        };
        where_sql = format!(" WHERE {expr} {fop} {}", metadata::lit(dialect, &val));
    }

    let mut order_sql = String::new();
    if !sort.is_empty() && names.contains(&sort.as_str()) {
        let d = if dir == "desc" { "DESC" } else { "ASC" };
        order_sql = format!(" ORDER BY {} {d}", metadata::qi(dialect, &sort));
    }

    let tbl_q = metadata::qi(dialect, tbl);
    let count_sql = format!("SELECT COUNT(*) AS c FROM {tbl_q}{where_sql}");
    let total: i64 = match dbr {
        DbRef::MySql(p, _) => sqlx::query(&count_sql).fetch_one(p).await?.get("c"),
        DbRef::Pg(p) => sqlx::query(&count_sql).fetch_one(p).await?.get("c"),
    };
    let pages = (total + PAGE_SIZE - 1) / PAGE_SIZE;
    let offset = page * PAGE_SIZE;

    let sel: Vec<String> = columns.iter().map(|c| cast_expr(dialect, c)).collect();
    let data_sql = format!(
        "SELECT {} FROM {tbl_q}{where_sql}{order_sql} LIMIT {PAGE_SIZE} OFFSET {offset}",
        sel.join(", ")
    );

    let pk_col = if pk.len() == 1 { pk[0].clone() } else { String::new() };
    let pk_index = if pk_col.is_empty() {
        -1
    } else {
        columns.iter().position(|c| c.name == pk_col).map(|i| i as i64).unwrap_or(-1)
    };

    let mut grid_rows = Vec::new();
    match dbr {
        DbRef::MySql(p, _) => {
            let rows = sqlx::query(&data_sql).fetch_all(p).await?;
            for r in rows {
                let cells: Vec<CellVal> = (0..columns.len()).map(|i| grid_cell_mysql(&r, i)).collect();
                let pkv = if pk_index >= 0 { cells[pk_index as usize].v.clone() } else { String::new() };
                grid_rows.push(GridRow { pk: pkv, cells });
            }
        }
        DbRef::Pg(p) => {
            let rows = sqlx::query(&data_sql).fetch_all(p).await?;
            for r in rows {
                let cells: Vec<CellVal> = (0..columns.len()).map(|i| grid_cell_pg(&r, i)).collect();
                let pkv = if pk_index >= 0 { cells[pk_index as usize].v.clone() } else { String::new() };
                grid_rows.push(GridRow { pk: pkv, cells });
            }
        }
    }

    let mut fq = Vec::new();
    if !fval.is_empty() && !fcol.is_empty() {
        fq.push(format!("fcol={}", urlenc(&fcol)));
        fq.push(format!("fop={}", urlenc(&fop)));
        fq.push(format!("fval={}", urlenc(&fval)));
    }
    let mut sq = Vec::new();
    if !sort.is_empty() {
        sq.push(format!("sort={}", urlenc(&sort)));
        sq.push(format!("dir={dir}"));
    }

    Ok(Grid {
        cols: columns.iter().map(|c| c.name.clone()).collect(),
        categories: columns.iter().map(|c| c.category.clone()).collect(),
        rows: grid_rows,
        pk_col,
        pk_index,
        page,
        pages,
        total,
        filter_qs: fq.join("&"),
        sort_qs: sq.join("&"),
        filter_col: fcol,
        filter_op: fop,
        filter_val: fval,
        sort,
        dir: dir.to_string(),
        new_row: q.new.as_deref() == Some("1"),
        edit_pk: q.edit.clone().unwrap_or_default(),
    })
}

async fn table_detail(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path((id, db, tbl)): Path<(i64, String, String)>,
    Query(q): Query<TableQuery>,
    Query(flash): Query<crate::connections::Flash>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let dbr = dbr_for(&state, &conn, &db).await?;
    let tab = q.tab.clone().unwrap_or_else(|| "struktur".to_string());
    let columns = metadata::get_columns(&dbr, &db, &tbl).await?;
    let pk = metadata::get_pk(&dbr, &db, &tbl).await?;

    let mut indexes = Vec::new();
    let mut fks = Vec::new();
    let mut refs = Vec::new();
    let mut ddl = String::new();
    let mut grid = None;

    match tab.as_str() {
        "data" => {
            grid = Some(build_grid(&dbr, &tbl, &columns, &pk, &q).await?);
        }
        "relasi" => {
            fks = metadata::get_fks(&dbr, &db, &tbl).await?;
            refs = metadata::get_fks_referencing(&dbr, &db, &tbl).await?;
        }
        "ddl" => {
            ddl = metadata::get_ddl(&dbr, &db, &tbl).await?;
        }
        _ => {
            indexes = metadata::get_indexes(&dbr, &db, &tbl).await?;
            fks = metadata::get_fks(&dbr, &db, &tbl).await?;
        }
    }

    let page = templates::TableDetail {
        conn_id: conn.id,
        conn_name: conn.name,
        db,
        tbl,
        tab,
        columns,
        indexes,
        fks,
        refs,
        ddl,
        pk_single: if pk.len() == 1 { pk[0].clone() } else { String::new() },
        grid,
        flash_ok: flash.msg,
        flash_err: flash.err,
    };
    Ok(Html(templates::render(&page)?).into_response())
}

// ------------------------------------------------------------- CRUD baris

fn row_fields(form: &HashMap<String, String>) -> Vec<(String, String)> {
    form.iter()
        .filter(|(k, _)| k.starts_with("f:"))
        .map(|(k, v)| (k[2..].to_string(), v.clone()))
        .collect()
}

/// Nilai per kolom: kosong + nullable => NULL; kosong + auto/default => skip.
fn build_values(
    dialect: &str,
    columns: &[ColumnInfo],
    fields: &[(String, String)],
    for_update: bool,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for c in columns {
        let raw = fields
            .iter()
            .find(|(k, _)| k == &c.name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        if raw.is_empty() {
            if !for_update && (c.extra.contains("auto_increment") || c.default.is_some()) {
                continue;
            }
            if c.nullable {
                out.push((metadata::qi(dialect, &c.name), "NULL".to_string()));
                continue;
            }
        }
        out.push((metadata::qi(dialect, &c.name), metadata::lit(dialect, &raw)));
    }
    out
}

async fn row_insert(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path((id, db, tbl)): Path<(i64, String, String)>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let to = format!(
        "/connections/{id}/databases/{}/tables/{}/?tab=data",
        urlenc(&db),
        urlenc(&tbl)
    );
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let dbr = dbr_for(&state, &conn, &db).await?;
    let columns = metadata::get_columns(&dbr, &db, &tbl).await?;
    let fields = row_fields(&form);
    let values = build_values(dbr.dialect(), &columns, &fields, false);
    if values.is_empty() {
        return Ok(flash_to(&ctx, &to, None, Some("Tidak ada nilai untuk di-insert")));
    }
    let (cols, vals): (Vec<String>, Vec<String>) = values.into_iter().unzip();
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        metadata::qi(dbr.dialect(), &tbl),
        cols.join(", "),
        vals.join(", ")
    );
    match dbr.execute(&sql).await {
        Ok(_) => {
            audit(&state, Some(id), "row_insert", &format!("INSERT pada {db}.{tbl}")).await?;
            Ok(flash_to(&ctx, &to, Some("Baris baru ditambahkan"), None))
        }
        Err(e) => Ok(flash_to(&ctx, &to, None, Some(&format!("Insert gagal: {e}")))),
    }
}

async fn row_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path((id, db, tbl)): Path<(i64, String, String)>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let to = format!(
        "/connections/{id}/databases/{}/tables/{}/?tab=data",
        urlenc(&db),
        urlenc(&tbl)
    );
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let Some(pk_val) = form.get("__pk") else {
        return Ok(flash_to(&ctx, &to, None, Some("Primary key tidak ditemukan")));
    };
    let pk_val = pk_val.clone();
    let dbr = dbr_for(&state, &conn, &db).await?;
    let columns = metadata::get_columns(&dbr, &db, &tbl).await?;
    let pk = metadata::get_pk(&dbr, &db, &tbl).await?;
    if pk.len() != 1 {
        return Ok(flash_to(
            &ctx,
            &to,
            None,
            Some("Edit hanya didukung untuk tabel dengan primary key satu kolom")),
        );
    }
    let fields = row_fields(&form);
    let values = build_values(dbr.dialect(), &columns, &fields, true);
    if values.is_empty() {
        return Ok(flash_to(&ctx, &to, None, Some("Tidak ada perubahan")));
    }
    let sets: Vec<String> = values
        .into_iter()
        .map(|(c, v)| format!("{c} = {v}"))
        .collect();
    let sql = format!(
        "UPDATE {} SET {} WHERE {} = {}",
        metadata::qi(dbr.dialect(), &tbl),
        sets.join(", "),
        metadata::qi(dbr.dialect(), &pk[0]),
        metadata::lit(dbr.dialect(), &pk_val)
    );
    match dbr.execute(&sql).await {
        Ok(n) => {
            audit(&state, Some(id), "row_update", &format!("UPDATE pada {db}.{tbl} ({n} baris)")).await?;
            Ok(flash_to(&ctx, &to, Some(&format!("Baris disimpan ({n} baris terpengaruh)")), None))
        }
        Err(e) => Ok(flash_to(&ctx, &to, None, Some(&format!("Update gagal: {e}")))),
    }
}

async fn row_delete(
    State(state): State<AppState>,
    Extension(ctx): Extension<SessionCtx>,
    Path((id, db, tbl)): Path<(i64, String, String)>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, AppError> {
    if let Some(r) = require_auth(&ctx) {
        return Ok(r);
    }
    let to = format!(
        "/connections/{id}/databases/{}/tables/{}/?tab=data",
        urlenc(&db),
        urlenc(&tbl)
    );
    let Some(conn) = load_conn(&state, id).await? else {
        return Ok(flash_to(&ctx, "/connections", None, Some("Koneksi tidak ditemukan")));
    };
    let Some(pk_val) = form.get("__pk") else {
        return Ok(flash_to(&ctx, &to, None, Some("Primary key tidak ditemukan")));
    };
    let pk_val = pk_val.clone();
    let dbr = dbr_for(&state, &conn, &db).await?;
    let pk = metadata::get_pk(&dbr, &db, &tbl).await?;
    if pk.len() != 1 {
        return Ok(flash_to(
            &ctx,
            &to,
            None,
            Some("Hapus hanya didukung untuk tabel dengan primary key satu kolom")),
        );
    }
    let sql = format!(
        "DELETE FROM {} WHERE {} = {}",
        metadata::qi(dbr.dialect(), &tbl),
        metadata::qi(dbr.dialect(), &pk[0]),
        metadata::lit(dbr.dialect(), &pk_val)
    );
    match dbr.execute(&sql).await {
        Ok(n) => {
            audit(&state, Some(id), "row_delete", &format!("DELETE pada {db}.{tbl} ({n} baris)")).await?;
            Ok(flash_to(&ctx, &to, Some(&format!("Baris dihapus ({n} baris)")), None))
        }
        Err(e) => Ok(flash_to(&ctx, &to, None, Some(&format!("Delete gagal: {e}")))),
    }
}
