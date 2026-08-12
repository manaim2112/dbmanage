use crate::error::AppError;
use askama::Template;

pub fn render<T: Template>(tpl: &T) -> Result<String, AppError> {
    Ok(tpl.render()?)
}

#[derive(Template)]
#[template(path = "login.html")]
pub struct Login {
    pub error: String,
}

#[derive(Template)]
#[template(path = "setup.html")]
pub struct Setup {
    pub error: String,
}

#[derive(Template)]
#[template(path = "totp.html")]
pub struct Totp {
    pub svg: String,
    pub secret: String,
    pub username: String,
    pub error: String,
}

#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct Dashboard {
    pub username: String,
    pub connections: i64,
    pub backups: i64,
    pub session_short: String,
}

#[derive(Template)]
#[template(path = "connections.html")]
pub struct Connections {
    pub rows: Vec<crate::connections::ConnRow>,
    pub flash_ok: Option<String>,
    pub flash_err: Option<String>,
}

#[derive(Template)]
#[template(path = "connection_form.html")]
pub struct ConnectionForm {
    pub is_edit: bool,
    pub error: String,
    pub name: String,
    pub db_type: String,
    pub host: String,
    pub port: String,
    pub username: String,
    pub back: String,
}

#[derive(Template)]
#[template(path = "databases.html")]
pub struct Databases {
    pub conn_name: String,
    pub db_type: String,
    pub dbs: Vec<String>,
    pub flash_ok: Option<String>,
    pub flash_err: Option<String>,
}

#[derive(Template)]
#[template(path = "users.html")]
pub struct Users {
    pub conn_name: String,
    pub users: Vec<(String, String)>,
    pub dbs: Vec<String>,
    pub flash_ok: Option<String>,
    pub flash_err: Option<String>,
}

#[derive(Template)]
#[template(path = "tables.html")]
pub struct Tables {
    pub conn_name: String,
    pub db: String,
    pub tables: Vec<crate::explorer::TableDisplay>,
    pub flash_ok: Option<String>,
    pub flash_err: Option<String>,
}

#[derive(Template)]
#[template(path = "table.html")]
pub struct TableDetail {
    pub conn_name: String,
    pub db: String,
    pub tbl: String,
    pub tab: String,
    pub columns: Vec<crate::metadata::ColumnInfo>,
    pub indexes: Vec<crate::metadata::IndexInfo>,
    pub fks: Vec<crate::metadata::FkInfo>,
    pub refs: Vec<crate::metadata::FkEdge>,
    pub ddl: String,
    pub grid: Option<crate::explorer::Grid>,
    pub flash_ok: Option<String>,
    pub flash_err: Option<String>,
}

#[derive(Template)]
#[template(path = "erd.html")]
pub struct Erd {
    pub conn_name: String,
    pub db: String,
    pub mermaid: String,
    pub has_fks: bool,
}

#[derive(Template)]
#[template(path = "query.html")]
pub struct Query {
    pub conn_name: String,
    pub db: String,
    pub sql: String,
    pub write: bool,
    pub result: Option<QueryResult>,
    pub error: String,
}

pub struct QueryResult {
    pub row_count: usize,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub affected: Option<u64>,
    pub duration_ms: u128,
}

#[derive(Template)]
#[template(path = "backups.html")]
pub struct Backups {
    pub configs: Vec<crate::backup::BackupRow>,
    pub history: Vec<crate::backup::HistoryRow>,
    pub flash_ok: Option<String>,
    pub flash_err: Option<String>,
}

#[derive(Template)]
#[template(path = "backup_form.html")]
pub struct BackupFormPage {
    pub is_edit: bool,
    pub error: String,
    pub connections: Vec<(i64, String, bool)>,
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
    pub enabled: bool,
}
