use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::error::AppResult;

pub type Db = SqlitePool;
pub type Tx = Transaction<'static, Sqlite>;

/// Open (and create/migrate) the database.
///
/// Memory-conscious settings: 4 connections, ~2 MB page cache each, WAL.
/// SQLite allows a single writer, so every state-changing operation uses
/// `immediate()` to take the write lock up front and serialize cleanly
/// instead of failing late with SQLITE_BUSY.
pub async fn connect(path: &str) -> AppResult<Db> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10))
        .pragma("cache_size", "-2000")
        .pragma("temp_store", "memory");
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(opts)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

/// Start a write transaction that holds the SQLite write lock immediately.
pub async fn immediate(pool: &Db) -> AppResult<Tx> {
    Ok(pool.begin_with("BEGIN IMMEDIATE").await?)
}
