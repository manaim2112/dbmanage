//! M3 — Metadata engine: query skema, index, FK, DDL, user, dan database
//! untuk MariaDB & PostgreSQL. Semua nilai dari UI di-embed sebagai literal
//! yang di-escape (bukan bind) agar satu builder bekerja di kedua dialek.

use anyhow::{bail, Result};
use sqlx::mysql::MySqlPool;
use sqlx::postgres::PgPool;
use sqlx::Row;

/// Target eksekusi: pool MySQL + nama database, atau pool Pg yang sudah
/// terikat ke satu database (PostgreSQL tidak bisa lintas-database per query).
#[derive(Clone)]
pub enum DbRef {
    MySql(MySqlPool, String),
    Pg(PgPool),
}

#[derive(Clone)]
pub struct TableInfo {
    pub name: String,
    pub engine: String,
    pub rows: i64,
    pub size: i64,
    pub collation: String,
    pub fk_count: i64,
}

#[derive(Clone)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub category: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub key: String,
    pub extra: String,
    pub comment: String,
}

#[derive(Clone)]
pub struct IndexInfo {
    pub name: String,
    pub unique: bool,
    pub columns: String,
    pub detail: String,
}

#[derive(Clone)]
pub struct FkInfo {
    pub name: String,
    pub column: String,
    pub ref_table: String,
    pub ref_column: String,
    pub on_delete: String,
    pub on_update: String,
}

#[derive(Clone)]
pub struct FkEdge {
    pub table: String,
    pub column: String,
    pub ref_table: String,
    pub ref_column: String,
}

impl DbRef {
    pub fn dialect(&self) -> &'static str {
        match self {
            DbRef::MySql(..) => "mariadb",
            DbRef::Pg(_) => "postgresql",
        }
    }

    pub async fn execute(&self, sql: &str) -> Result<u64> {
        let n = match self {
            DbRef::MySql(p, _) => sqlx::query(sql).execute(p).await?.rows_affected(),
            DbRef::Pg(p) => sqlx::query(sql).execute(p).await?.rows_affected(),
        };
        Ok(n)
    }
}

/// Quote identifier sesuai dialek.
pub fn qi(dialect: &str, ident: &str) -> String {
    match dialect {
        "mariadb" => format!("`{}`", ident.replace('`', "``")),
        _ => format!("\"{}\"", ident.replace('"', "\"\"")),
    }
}

/// Escape string menjadi SQL literal aman.
pub fn lit(dialect: &str, s: &str) -> String {
    match dialect {
        "mariadb" => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        _ => format!("'{}'", s.replace('\'', "''")),
    }
}

pub fn type_category(t: &str) -> &'static str {
    let t = t.to_lowercase();
    if t.contains("bool") {
        "bool"
    } else if t.contains("uuid") {
        "uuid"
    } else if t.contains("json") {
        "json"
    } else if t.contains("blob")
        || t.contains("binary")
        || t.contains("bytea")
        || t.contains("bit")
    {
        "bin"
    } else if t.contains("int") || t.contains("serial") {
        "int"
    } else if t.contains("decimal")
        || t.contains("numeric")
        || t.contains("float")
        || t.contains("double")
        || t.contains("real")
        || t.contains("money")
    {
        "dec"
    } else if t.contains("date") || t.contains("time") {
        "time"
    } else if t.contains("char") || t.contains("text") || t.contains("enum") || t.contains("set") {
        "str"
    } else {
        "other"
    }
}

// ---------------------------------------------------------------- database

pub async fn list_databases(dbr: &DbRef) -> Result<Vec<String>> {
    let mut out = Vec::new();
    match dbr {
        DbRef::MySql(p, _) => {
            let rows = sqlx::query("SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME")
                .fetch_all(p)
                .await?;
            for r in rows {
                let name: String = r.get("SCHEMA_NAME");
                if !matches!(
                    name.as_str(),
                    "information_schema" | "mysql" | "performance_schema" | "sys"
                ) {
                    out.push(name);
                }
            }
        }
        DbRef::Pg(p) => {
            let rows = sqlx::query(
                "SELECT datname FROM pg_database WHERE datistemplate = false ORDER BY datname",
            )
            .fetch_all(p)
            .await?;
            for r in rows {
                out.push(r.get("datname"));
            }
        }
    }
    Ok(out)
}

pub async fn create_database(dbr: &DbRef, name: &str) -> Result<()> {
    let sql = match dbr {
        DbRef::MySql(..) => format!(
            "CREATE DATABASE {} CHARACTER SET utf8mb4",
            qi("mariadb", name)
        ),
        DbRef::Pg(_) => format!("CREATE DATABASE {}", qi("postgresql", name)),
    };
    dbr.execute(&sql).await?;
    Ok(())
}

pub async fn drop_database(dbr: &DbRef, name: &str) -> Result<()> {
    let sql = format!("DROP DATABASE {}", qi(dbr.dialect(), name));
    dbr.execute(&sql).await?;
    Ok(())
}

// ------------------------------------------------------------------- user

pub async fn list_users(dbr: &DbRef) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    match dbr {
        DbRef::MySql(p, _) => {
            let rows = sqlx::query("SELECT User, Host FROM mysql.user ORDER BY User, Host")
                .fetch_all(p)
                .await?;
            for r in rows {
                out.push((r.get("User"), r.get("Host")));
            }
        }
        DbRef::Pg(p) => {
            let rows = sqlx::query("SELECT usename FROM pg_user ORDER BY usename")
                .fetch_all(p)
                .await?;
            for r in rows {
                let u: String = r.get("usename");
                out.push((u, "*".to_string()));
            }
        }
    }
    Ok(out)
}

/// Buat user + password, lalu grant privilege ke satu database.
pub async fn create_user(
    dbr: &DbRef,
    username: &str,
    password: &str,
    grant_db: &str,
    priv_level: &str,
) -> Result<()> {
    match dbr {
        DbRef::MySql(..) => {
            let u = lit("mariadb", username);
            dbr.execute(&format!("CREATE USER {u}@'%' IDENTIFIED BY {}", lit("mariadb", password)))
                .await?;
            let privs = match priv_level {
                "SELECT" => "SELECT",
                "RW" => "SELECT, INSERT, UPDATE, DELETE",
                _ => "ALL PRIVILEGES",
            };
            if !grant_db.is_empty() {
                dbr.execute(&format!(
                    "GRANT {privs} ON {}.* TO {u}@'%'",
                    qi("mariadb", grant_db)
                ))
                .await?;
            }
            dbr.execute("FLUSH PRIVILEGES").await?;
        }
        DbRef::Pg(_) => {
            let u = qi("postgresql", username);
            dbr.execute(&format!(
                "CREATE USER {u} WITH PASSWORD {}",
                lit("postgresql", password)
            ))
            .await?;
            if !grant_db.is_empty() {
                let db = qi("postgresql", grant_db);
                match priv_level {
                    "SELECT" => {
                        dbr.execute(&format!("GRANT CONNECT ON DATABASE {db} TO {u}"))
                            .await?;
                    }
                    _ => {
                        dbr.execute(&format!("GRANT ALL PRIVILEGES ON DATABASE {db} TO {u}"))
                            .await?;
                    }
                }
            }
        }
    }
    Ok(())
}

pub async fn drop_user(dbr: &DbRef, username: &str, host: &str) -> Result<()> {
    let sql = match dbr {
        DbRef::MySql(..) => format!(
            "DROP USER {}@{}",
            lit("mariadb", username),
            lit("mariadb", host)
        ),
        DbRef::Pg(_) => format!("DROP USER {}", qi("postgresql", username)),
    };
    dbr.execute(&sql).await?;
    Ok(())
}

// ------------------------------------------------------------------ tabel

pub async fn list_tables(dbr: &DbRef, db: &str) -> Result<Vec<TableInfo>> {
    let mut out = Vec::new();
    match dbr {
        DbRef::MySql(p, _) => {
            let rows = sqlx::query(
                "SELECT TABLE_NAME, ENGINE, TABLE_ROWS, DATA_LENGTH, INDEX_LENGTH, TABLE_COLLATION
                 FROM information_schema.TABLES
                 WHERE TABLE_SCHEMA = ? AND TABLE_TYPE = 'BASE TABLE'
                 ORDER BY TABLE_NAME",
            )
            .bind(db)
            .fetch_all(p)
            .await?;
            let fk_rows = sqlx::query(
                "SELECT TABLE_NAME, COUNT(DISTINCT CONSTRAINT_NAME) AS fks
                 FROM information_schema.TABLE_CONSTRAINTS
                 WHERE TABLE_SCHEMA = ? AND CONSTRAINT_TYPE = 'FOREIGN KEY'
                 GROUP BY TABLE_NAME",
            )
            .bind(db)
            .fetch_all(p)
            .await?;
            let fk_map: std::collections::HashMap<String, i64> = fk_rows
                .into_iter()
                .map(|r| (r.get::<String, _>("TABLE_NAME"), r.get::<i64, _>("fks")))
                .collect();
            for r in rows {
                let name: String = r.get("TABLE_NAME");
                let data: Option<i64> = r.get("DATA_LENGTH");
                let idx: Option<i64> = r.get("INDEX_LENGTH");
                out.push(TableInfo {
                    fk_count: fk_map.get(&name).copied().unwrap_or(0),
                    name,
                    engine: r.get::<Option<String>, _>("ENGINE").unwrap_or_default(),
                    rows: r.get::<Option<i64>, _>("TABLE_ROWS").unwrap_or(0),
                    size: data.unwrap_or(0) + idx.unwrap_or(0),
                    collation: r
                        .get::<Option<String>, _>("TABLE_COLLATION")
                        .unwrap_or_default(),
                });
            }
        }
        DbRef::Pg(p) => {
            let rows = sqlx::query(
                "SELECT c.relname AS name,
                        GREATEST(c.reltuples::bigint, 0) AS rows,
                        pg_total_relation_size(c.oid) AS size
                 FROM pg_class c
                 JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname = 'public' AND c.relkind = 'r'
                 ORDER BY c.relname",
            )
            .fetch_all(p)
            .await?;
            let fk_rows = sqlx::query(
                "SELECT tc.table_name, COUNT(*) AS fks
                 FROM information_schema.table_constraints tc
                 WHERE tc.constraint_type = 'FOREIGN KEY' AND tc.table_schema = 'public'
                 GROUP BY tc.table_name",
            )
            .fetch_all(p)
            .await?;
            let fk_map: std::collections::HashMap<String, i64> = fk_rows
                .into_iter()
                .map(|r| (r.get::<String, _>("table_name"), r.get::<i64, _>("fks")))
                .collect();
            for r in rows {
                let name: String = r.get("name");
                out.push(TableInfo {
                    fk_count: fk_map.get(&name).copied().unwrap_or(0),
                    name,
                    engine: "heap".to_string(),
                    rows: r.get::<Option<i64>, _>("rows").unwrap_or(0),
                    size: r.get::<Option<i64>, _>("size").unwrap_or(0),
                    collation: String::new(),
                });
            }
        }
    }
    Ok(out)
}

// ----------------------------------------------------------------- kolom

pub async fn get_columns(dbr: &DbRef, db: &str, tbl: &str) -> Result<Vec<ColumnInfo>> {
    let mut out = Vec::new();
    match dbr {
        DbRef::MySql(p, _) => {
            let rows = sqlx::query(
                "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, COLUMN_KEY, EXTRA, COLUMN_COMMENT
                 FROM information_schema.COLUMNS
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
                 ORDER BY ORDINAL_POSITION",
            )
            .bind(db)
            .bind(tbl)
            .fetch_all(p)
            .await?;
            for r in rows {
                let dtype: String = r.get("COLUMN_TYPE");
                let key: String = r.get("COLUMN_KEY");
                out.push(ColumnInfo {
                    name: r.get("COLUMN_NAME"),
                    category: type_category(&dtype).to_string(),
                    data_type: dtype,
                    nullable: r.get::<String, _>("IS_NULLABLE") == "YES",
                    default: r.get("COLUMN_DEFAULT"),
                    key: match key.as_str() {
                        "PRI" => "PK".to_string(),
                        "UNI" => "UQ".to_string(),
                        "MUL" => "IDX".to_string(),
                        _ => String::new(),
                    },
                    extra: r.get("EXTRA"),
                    comment: r.get("COLUMN_COMMENT"),
                });
            }
        }
        DbRef::Pg(p) => {
            let rows = sqlx::query(
                "SELECT column_name, data_type, udt_name, is_nullable, column_default,
                        character_maximum_length, numeric_precision, numeric_scale
                 FROM information_schema.columns
                 WHERE table_schema = 'public' AND table_name = ?
                 ORDER BY ordinal_position",
            )
            .bind(tbl)
            .fetch_all(p)
            .await?;
            for r in rows {
                let data_type: String = r.get("data_type");
                let udt: String = r.get("udt_name");
                let maxlen: Option<i64> = r.get("character_maximum_length");
                let prec: Option<i64> = r.get("numeric_precision");
                let scale: Option<i64> = r.get("numeric_scale");
                let display = match (maxlen, prec, scale) {
                    (Some(m), _, _) => format!("{udt}({m})"),
                    (_, Some(p), Some(s)) if data_type == "numeric" => format!("numeric({p},{s})"),
                    _ => udt.clone(),
                };
                out.push(ColumnInfo {
                    name: r.get("column_name"),
                    category: type_category(&udt).to_string(),
                    data_type: display,
                    nullable: r.get::<String, _>("is_nullable") == "YES",
                    default: r.get("column_default"),
                    key: String::new(),
                    extra: r
                        .get::<Option<String>, _>("column_default")
                        .filter(|d| d.contains("nextval"))
                        .map(|_| "auto_increment".to_string())
                        .unwrap_or_default(),
                    comment: String::new(),
                });
            }
        }
    }
    Ok(out)
}

pub async fn get_pk(dbr: &DbRef, db: &str, tbl: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    match dbr {
        DbRef::MySql(p, _) => {
            let rows = sqlx::query(
                "SELECT COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND CONSTRAINT_NAME = 'PRIMARY'
                 ORDER BY ORDINAL_POSITION",
            )
            .bind(db)
            .bind(tbl)
            .fetch_all(p)
            .await?;
            for r in rows {
                out.push(r.get("COLUMN_NAME"));
            }
        }
        DbRef::Pg(p) => {
            let rows = sqlx::query(
                "SELECT a.attname
                 FROM pg_index i
                 JOIN pg_class c ON c.oid = i.indrelid
                 JOIN pg_namespace n ON n.oid = c.relnamespace
                 JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ANY(i.indkey)
                 WHERE n.nspname = 'public' AND c.relname = ? AND i.indisprimary",
            )
            .bind(tbl)
            .fetch_all(p)
            .await?;
            for r in rows {
                out.push(r.get("attname"));
            }
        }
    }
    Ok(out)
}

pub async fn get_indexes(dbr: &DbRef, db: &str, tbl: &str) -> Result<Vec<IndexInfo>> {
    let mut out = Vec::new();
    match dbr {
        DbRef::MySql(p, _) => {
            let rows = sqlx::query(
                "SELECT INDEX_NAME, NON_UNIQUE, INDEX_TYPE, COLUMN_NAME
                 FROM information_schema.STATISTICS
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
                 ORDER BY INDEX_NAME, SEQ_IN_INDEX",
            )
            .bind(db)
            .bind(tbl)
            .fetch_all(p)
            .await?;
            let mut current: Option<IndexInfo> = None;
            for r in rows {
                let name: String = r.get("INDEX_NAME");
                let col: String = r.get("COLUMN_NAME");
                let non_unique: i64 = r.get("NON_UNIQUE");
                let itype: String = r.get("INDEX_TYPE");
                match current.as_mut() {
                    Some(c) if c.name == name => {
                        c.columns.push_str(", ");
                        c.columns.push_str(&col);
                    }
                    _ => {
                        if let Some(c) = current.take() {
                            out.push(c);
                        }
                        current = Some(IndexInfo {
                            name,
                            unique: non_unique == 0,
                            columns: col,
                            detail: itype,
                        });
                    }
                }
            }
            if let Some(c) = current {
                out.push(c);
            }
        }
        DbRef::Pg(p) => {
            let rows = sqlx::query(
                "SELECT indexname, indexdef FROM pg_indexes
                 WHERE schemaname = 'public' AND tablename = ?
                 ORDER BY indexname",
            )
            .bind(tbl)
            .fetch_all(p)
            .await?;
            for r in rows {
                let def: String = r.get("indexdef");
                let cols = def
                    .split('(')
                    .nth(1)
                    .and_then(|s| s.rsplit(')').nth(1))
                    .unwrap_or("")
                    .to_string();
                out.push(IndexInfo {
                    name: r.get("indexname"),
                    unique: def.starts_with("CREATE UNIQUE"),
                    columns: cols,
                    detail: "btree".to_string(),
                });
            }
        }
    }
    Ok(out)
}

pub async fn get_fks(dbr: &DbRef, db: &str, tbl: &str) -> Result<Vec<FkInfo>> {
    let mut out = Vec::new();
    match dbr {
        DbRef::MySql(p, _) => {
            let rows = sqlx::query(
                "SELECT kcu.CONSTRAINT_NAME, kcu.COLUMN_NAME, kcu.REFERENCED_TABLE_NAME,
                        kcu.REFERENCED_COLUMN_NAME, rc.DELETE_RULE, rc.UPDATE_RULE
                 FROM information_schema.KEY_COLUMN_USAGE kcu
                 JOIN information_schema.REFERENTIAL_CONSTRAINTS rc
                   ON rc.CONSTRAINT_SCHEMA = kcu.TABLE_SCHEMA
                  AND rc.CONSTRAINT_NAME = kcu.CONSTRAINT_NAME
                  AND rc.TABLE_NAME = kcu.TABLE_NAME
                 WHERE kcu.TABLE_SCHEMA = ? AND kcu.TABLE_NAME = ?
                   AND kcu.REFERENCED_TABLE_NAME IS NOT NULL",
            )
            .bind(db)
            .bind(tbl)
            .fetch_all(p)
            .await?;
            for r in rows {
                out.push(FkInfo {
                    name: r.get("CONSTRAINT_NAME"),
                    column: r.get("COLUMN_NAME"),
                    ref_table: r.get("REFERENCED_TABLE_NAME"),
                    ref_column: r.get("REFERENCED_COLUMN_NAME"),
                    on_delete: r.get("DELETE_RULE"),
                    on_update: r.get("UPDATE_RULE"),
                });
            }
        }
        DbRef::Pg(p) => {
            let rows = sqlx::query(
                "SELECT tc.constraint_name, kcu.column_name, ccu.table_name AS ref_table,
                        ccu.column_name AS ref_column, rc.delete_rule, rc.update_rule
                 FROM information_schema.table_constraints tc
                 JOIN information_schema.key_column_usage kcu
                   ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema
                 JOIN information_schema.referential_constraints rc
                   ON rc.constraint_name = tc.constraint_name AND rc.constraint_schema = tc.table_schema
                 JOIN information_schema.constraint_column_usage ccu
                   ON ccu.constraint_name = tc.constraint_name AND ccu.constraint_schema = tc.table_schema
                 WHERE tc.constraint_type = 'FOREIGN KEY'
                   AND tc.table_schema = 'public' AND tc.table_name = ?",
            )
            .bind(tbl)
            .fetch_all(p)
            .await?;
            for r in rows {
                out.push(FkInfo {
                    name: r.get("constraint_name"),
                    column: r.get("column_name"),
                    ref_table: r.get("ref_table"),
                    ref_column: r.get("ref_column"),
                    on_delete: r.get("delete_rule"),
                    on_update: r.get("update_rule"),
                });
            }
        }
    }
    Ok(out)
}

/// FK dari tabel lain yang menunjuk ke `tbl` (untuk reverse lookup row detail).
pub async fn get_fks_referencing(dbr: &DbRef, db: &str, tbl: &str) -> Result<Vec<FkEdge>> {
    let mut out = Vec::new();
    match dbr {
        DbRef::MySql(p, _) => {
            let rows = sqlx::query(
                "SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE
                 WHERE REFERENCED_TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME = ?",
            )
            .bind(db)
            .bind(tbl)
            .fetch_all(p)
            .await?;
            for r in rows {
                out.push(FkEdge {
                    table: r.get("TABLE_NAME"),
                    column: r.get("COLUMN_NAME"),
                    ref_table: tbl.to_string(),
                    ref_column: String::new(),
                });
            }
        }
        DbRef::Pg(p) => {
            let rows = sqlx::query(
                "SELECT kcu.table_name, kcu.column_name
                 FROM information_schema.referential_constraints rc
                 JOIN information_schema.key_column_usage kcu
                   ON kcu.constraint_name = rc.constraint_name AND kcu.table_schema = rc.constraint_schema
                 JOIN information_schema.constraint_column_usage ccu
                   ON ccu.constraint_name = rc.constraint_name AND ccu.constraint_schema = rc.constraint_schema
                 WHERE kcu.table_schema = 'public' AND ccu.table_name = ?",
            )
            .bind(tbl)
            .fetch_all(p)
            .await?;
            for r in rows {
                out.push(FkEdge {
                    table: r.get("table_name"),
                    column: r.get("column_name"),
                    ref_table: tbl.to_string(),
                    ref_column: String::new(),
                });
            }
        }
    }
    Ok(out)
}

/// Semua FK dalam satu database (untuk ERD).
pub async fn all_fks(dbr: &DbRef, db: &str) -> Result<Vec<FkEdge>> {
    let mut out = Vec::new();
    match dbr {
        DbRef::MySql(p, _) => {
            let rows = sqlx::query(
                "SELECT TABLE_NAME, COLUMN_NAME, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME
                 FROM information_schema.KEY_COLUMN_USAGE
                 WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL",
            )
            .bind(db)
            .fetch_all(p)
            .await?;
            for r in rows {
                out.push(FkEdge {
                    table: r.get("TABLE_NAME"),
                    column: r.get("COLUMN_NAME"),
                    ref_table: r.get("REFERENCED_TABLE_NAME"),
                    ref_column: r.get("REFERENCED_COLUMN_NAME"),
                });
            }
        }
        DbRef::Pg(p) => {
            let rows = sqlx::query(
                "SELECT kcu.table_name, kcu.column_name, ccu.table_name AS ref_table,
                        ccu.column_name AS ref_column
                 FROM information_schema.referential_constraints rc
                 JOIN information_schema.key_column_usage kcu
                   ON kcu.constraint_name = rc.constraint_name AND kcu.table_schema = rc.constraint_schema
                 JOIN information_schema.constraint_column_usage ccu
                   ON ccu.constraint_name = rc.constraint_name AND ccu.constraint_schema = rc.constraint_schema
                 WHERE kcu.table_schema = 'public'",
            )
            .fetch_all(p)
            .await?;
            for r in rows {
                out.push(FkEdge {
                    table: r.get("table_name"),
                    column: r.get("column_name"),
                    ref_table: r.get("ref_table"),
                    ref_column: r.get("ref_column"),
                });
            }
        }
    }
    Ok(out)
}

/// PK semua tabel sekaligus (untuk ERD).
pub async fn pk_map(dbr: &DbRef, db: &str) -> Result<std::collections::HashMap<String, Vec<String>>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    match dbr {
        DbRef::MySql(p, _) => {
            let rows = sqlx::query(
                "SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE
                 WHERE TABLE_SCHEMA = ? AND CONSTRAINT_NAME = 'PRIMARY'
                 ORDER BY TABLE_NAME, ORDINAL_POSITION",
            )
            .bind(db)
            .fetch_all(p)
            .await?;
            for r in rows {
                out.entry(r.get("TABLE_NAME"))
                    .or_default()
                    .push(r.get("COLUMN_NAME"));
            }
        }
        DbRef::Pg(p) => {
            let rows = sqlx::query(
                "SELECT c.relname AS table_name, a.attname AS column_name
                 FROM pg_index i
                 JOIN pg_class c ON c.oid = i.indrelid
                 JOIN pg_namespace n ON n.oid = c.relnamespace
                 JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ANY(i.indkey)
                 WHERE n.nspname = 'public' AND i.indisprimary
                 ORDER BY c.relname",
            )
            .fetch_all(p)
            .await?;
            for r in rows {
                out.entry(r.get("table_name"))
                    .or_default()
                    .push(r.get("column_name"));
            }
        }
    }
    Ok(out)
}

pub async fn get_ddl(dbr: &DbRef, _db: &str, tbl: &str) -> Result<String> {
    match dbr {
        DbRef::MySql(p, _) => {
            let sql = format!("SHOW CREATE TABLE {}", qi("mariadb", tbl));
            let row = sqlx::query(&sql).fetch_one(p).await?;
            Ok(row.get::<String, _>(1))
        }
        DbRef::Pg(p) => {
            // PostgreSQL tidak punya SHOW CREATE TABLE — generate pendekatan dari kolom.
            let rows = sqlx::query(
                "SELECT column_name, data_type, udt_name, is_nullable, column_default, character_maximum_length
                 FROM information_schema.columns
                 WHERE table_schema = 'public' AND table_name = ?
                 ORDER BY ordinal_position",
            )
            .bind(tbl)
            .fetch_all(p)
            .await?;
            let mut lines = Vec::new();
            for r in rows {
                let name: String = r.get("column_name");
                let udt: String = r.get("udt_name");
                let maxlen: Option<i64> = r.get("character_maximum_length");
                let nullable: String = r.get("is_nullable");
                let def: Option<String> = r.get("column_default");
                let t = match maxlen {
                    Some(m) => format!("{udt}({m})"),
                    None => udt,
                };
                let mut line = format!("    {} {}", qi("postgresql", &name), t);
                if let Some(d) = def {
                    line.push_str(&format!(" DEFAULT {d}"));
                }
                if nullable == "NO" {
                    line.push_str(" NOT NULL");
                }
                lines.push(line);
            }
            Ok(format!(
                "-- DDL pendekatan (dihasilkan dari metadata)\nCREATE TABLE public.{} (\n{}\n);",
                qi("postgresql", tbl),
                lines.join(",\n")
            ))
        }
    }
}

/// Escape nama untuk entity mermaid (hanya alnum + underscore).
pub fn mermaid_ident(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if s.is_empty() || s.chars().next().unwrap().is_ascii_digit() {
        format!("t_{s}")
    } else {
        s
    }
}

/// Validasi nama identifier dari user (database/user/tabel baru).
pub fn valid_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.chars().next().unwrap().is_ascii_digit()
}

pub fn assert_identifier(name: &str, label: &str) -> Result<()> {
    if !valid_identifier(name) {
        bail!("{label} tidak valid (1-63 karakter, hanya huruf/angka/underscore, tidak boleh diawali angka)");
    }
    Ok(())
}
