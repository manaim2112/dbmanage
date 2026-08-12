use crate::config::Config;
use crate::pools::PoolManager;
use aes_gcm::Aes256Gcm;
use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<Config>,
    pub pools: PoolManager,
    pub cipher: Arc<Aes256Gcm>,
}

impl AppState {
    pub fn new(db: SqlitePool, config: Config, pools: PoolManager, cipher: Arc<Aes256Gcm>) -> Self {
        Self {
            db,
            config: Arc::new(config),
            pools,
            cipher,
        }
    }
}

#[derive(Clone)]
pub struct SessionCtx {
    pub id: String,
    pub status: String,
    pub user_id: Option<i64>,
}
