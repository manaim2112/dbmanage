mod auth;
mod config;
mod connections;
mod crypto;
mod db;
mod error;
mod pools;
mod session;
mod state;
mod templates;
mod totp;

use axum::{routing::get, Router};
use tower_http::services::ServeDir;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = config::Config::from_env()?;
    let pool = db::init(&config).await?;
    let cipher = std::sync::Arc::new(crypto::cipher_from_secret(&config.secret_key));
    let state = state::AppState::new(
        pool,
        config.clone(),
        pools::PoolManager::default(),
        cipher,
    );

    let app = Router::new()
        .route("/", get(auth::root))
        .nest("/app", auth::app_router(state.clone()))
        .nest_service("/static", ServeDir::new("static"))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            session::session_resolver,
        ))
        .with_state(state.clone());

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("DBManage listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
