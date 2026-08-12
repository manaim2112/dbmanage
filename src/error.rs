use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

pub struct AppError(pub anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError(e.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!("error: {:#}", self.0);
        let msg = escape(&self.0.to_string());
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html(format!(
                "<!doctype html><html><head><meta charset=utf-8><title>Error — DBManage</title></head>\
                <body style=\"background:#0b0e14;color:#e6e9f0;font-family:monospace;padding:32px\">\
                <h3>Terjadi kesalahan</h3><pre>{msg}</pre>\
                <p><a style=\"color:#6366f1\" href=\"/\">Kembali ke awal</a></p></body></html>"
            )),
        )
            .into_response()
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
