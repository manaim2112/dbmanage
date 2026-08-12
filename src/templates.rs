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
