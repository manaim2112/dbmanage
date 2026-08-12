use crate::config::Config;
use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS users (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        username TEXT NOT NULL UNIQUE,
        password_hash TEXT NOT NULL,
        totp_secret TEXT,
        totp_enabled INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        created_at INTEGER NOT NULL,
        last_active INTEGER NOT NULL,
        status TEXT NOT NULL DEFAULT 'pending',
        user_id INTEGER REFERENCES users(id)
    )",
    "CREATE TABLE IF NOT EXISTS connections (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        db_type TEXT NOT NULL CHECK(db_type IN ('mariadb', 'postgresql')),
        host TEXT NOT NULL,
        port INTEGER NOT NULL,
        username TEXT NOT NULL,
        password_encrypted TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS backup_configs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        connection_id INTEGER NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
        database_name TEXT NOT NULL,
        provider TEXT NOT NULL CHECK(provider IN ('s3', 'gdrive')),
        config_json TEXT NOT NULL,
        schedule_cron TEXT,
        retention_days INTEGER NOT NULL DEFAULT 30,
        enabled INTEGER NOT NULL DEFAULT 1,
        created_at INTEGER NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS backup_history (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        backup_config_id INTEGER REFERENCES backup_configs(id) ON DELETE SET NULL,
        filename TEXT NOT NULL,
        size_bytes INTEGER,
        status TEXT NOT NULL CHECK(status IN ('running', 'success', 'failed')),
        provider TEXT NOT NULL,
        remote_path TEXT,
        error_message TEXT,
        started_at INTEGER NOT NULL,
        finished_at INTEGER
    )",
    "CREATE TABLE IF NOT EXISTS audit_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        connection_id INTEGER REFERENCES connections(id) ON DELETE SET NULL,
        action TEXT NOT NULL,
        detail TEXT NOT NULL,
        executed_at INTEGER NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_sessions_last_active ON sessions (last_active)",
];

pub async fn init(config: &Config) -> Result<SqlitePool> {
    std::fs::create_dir_all(&config.data_dir)?;
    let opts = SqliteConnectOptions::new()
        .filename(config.db_path())
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await?;
    for migration in MIGRATIONS {
        sqlx::query(migration).execute(&pool).await?;
    }
    tracing::info!("internal database ready at {}", config.db_path().display());
    Ok(pool)
}
