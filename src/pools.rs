//! Connection pool per server database (MariaDB & PostgreSQL).

use anyhow::{bail, Result};
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::Row;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Clone)]
pub enum PoolHandle {
    MySql(MySqlPool),
    Postgres(PgPool),
}

pub struct Status {
    pub online: bool,
    pub version: String,
    pub latency_ms: u128,
}

impl PoolHandle {
    pub async fn ping(&self) -> Status {
        let start = std::time::Instant::now();
        let version = match self {
            PoolHandle::MySql(p) => ping_mysql(p).await,
            PoolHandle::Postgres(p) => ping_pg(p).await,
        };
        let latency_ms = start.elapsed().as_millis();
        match version {
            Some(v) => Status {
                online: true,
                version: v,
                latency_ms,
            },
            None => Status {
                online: false,
                version: String::new(),
                latency_ms,
            },
        }
    }

    pub async fn close(&self) {
        match self {
            PoolHandle::MySql(p) => p.close().await,
            PoolHandle::Postgres(p) => p.close().await,
        }
    }
}

async fn ping_mysql(p: &MySqlPool) -> Option<String> {
    let row = tokio::time::timeout(
        Duration::from_secs(5),
        sqlx::query("SELECT VERSION() AS v").fetch_one(p),
    )
    .await
    .ok()?
    .ok()?;
    Some(shorten_version(row.get::<String, _>("v")))
}

async fn ping_pg(p: &PgPool) -> Option<String> {
    let row = tokio::time::timeout(
        Duration::from_secs(5),
        sqlx::query("SELECT VERSION() AS v").fetch_one(p),
    )
    .await
    .ok()?
    .ok()?;
    Some(shorten_version(row.get::<String, _>("v")))
}

fn shorten_version(v: String) -> String {
    let v = v.split(',').next().unwrap_or("").trim().to_string();
    v.chars().take(40).collect()
}

pub async fn create_pool(
    db_type: &str,
    host: &str,
    port: u16,
    username: &str,
    password: &str,
) -> Result<PoolHandle> {
    match db_type {
        "mariadb" => {
            let opts = MySqlConnectOptions::new()
                .host(host)
                .port(port)
                .username(username)
                .password(password);
            let pool = MySqlPoolOptions::new()
                .min_connections(2)
                .max_connections(10)
                .acquire_timeout(Duration::from_secs(5))
                .connect_with(opts)
                .await?;
            Ok(PoolHandle::MySql(pool))
        }
        "postgresql" => {
            let opts = PgConnectOptions::new()
                .host(host)
                .port(port)
                .username(username)
                .password(password)
                .database("postgres");
            let pool = PgPoolOptions::new()
                .min_connections(2)
                .max_connections(10)
                .acquire_timeout(Duration::from_secs(5))
                .connect_with(opts)
                .await?;
            Ok(PoolHandle::Postgres(pool))
        }
        other => bail!("tipe database tidak dikenal: {other}"),
    }
}

/// Registry pool per connection id; pool dibuat lazy dan di-reuse antar request.
#[derive(Clone, Default)]
pub struct PoolManager {
    pools: Arc<RwLock<HashMap<i64, PoolHandle>>>,
    /// Sub-pool PostgreSQL per database (PG tidak bisa query lintas database).
    db_pools: Arc<RwLock<HashMap<(i64, String), PgPool>>>,
}

impl PoolManager {
    pub async fn get(&self, id: i64) -> Option<PoolHandle> {
        self.pools.read().await.get(&id).cloned()
    }

    pub async fn insert(&self, id: i64, handle: PoolHandle) {
        if let Some(old) = self.pools.write().await.insert(id, handle) {
            old.close().await;
        }
    }

    pub async fn remove(&self, id: i64) {
        if let Some(old) = self.pools.write().await.remove(&id) {
            old.close().await;
        }
        let mut db_pools = self.db_pools.write().await;
        let keys: Vec<(i64, String)> = db_pools
            .keys()
            .filter(|(cid, _)| *cid == id)
            .cloned()
            .collect();
        for key in keys {
            if let Some(pool) = db_pools.remove(&key) {
                pool.close().await;
            }
        }
    }

    /// Pool untuk database PostgreSQL tertentu pada connection `id`.
    pub async fn pg_pool(
        &self,
        id: i64,
        database: &str,
        host: &str,
        port: u16,
        username: &str,
        password: &str,
    ) -> Result<PgPool> {
        let key = (id, database.to_string());
        if let Some(pool) = self.db_pools.read().await.get(&key) {
            return Ok(pool.clone());
        }
        let opts = PgConnectOptions::new()
            .host(host)
            .port(port)
            .username(username)
            .password(password)
            .database(database);
        let pool = PgPoolOptions::new()
            .min_connections(1)
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(opts)
            .await?;
        self.db_pools.write().await.insert(key, pool.clone());
        Ok(pool)
    }

    /// Tutup sub-pool database PostgreSQL tertentu (dipakai sebelum DROP DATABASE).
    pub async fn close_pg_db(&self, id: i64, database: &str) {
        if let Some(p) = self.db_pools.write().await.remove(&(id, database.to_string())) {
            p.close().await;
        }
    }
}
