//! Diesel connection pool management for SQLite.
//!
//! Since diesel-async only supports Postgres/MySQL, SQLite operations
//! use sync Diesel with r2d2 connection pooling, wrapped in spawn_blocking.

use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool, PooledConnection};
use std::path::Path;
use std::time::Duration;

/// Diesel error type alias.
pub type DieselError = diesel::result::Error;

/// r2d2 pool error type alias.
pub type R2D2Error = diesel::r2d2::PoolError;

/// Connection pool for SQLite using r2d2.
pub type SqlitePool = Pool<ConnectionManager<SqliteConnection>>;

/// Pooled connection type.
pub type PooledConn = PooledConnection<ConnectionManager<SqliteConnection>>;

/// Create a Diesel connection pool for SQLite.
///
/// Returns an r2d2 pool that can be used with spawn_blocking for async operations.
pub fn create_diesel_pool(db_path: &Path) -> Result<SqlitePool, R2D2Error> {
    let db_url = format!("sqlite:{}", db_path.display());
    create_diesel_pool_from_url(&db_url)
}

/// Create a Diesel connection pool from a database URL.
pub fn create_diesel_pool_from_url(database_url: &str) -> Result<SqlitePool, R2D2Error> {
    // Strip "sqlite:" prefix if present for Diesel
    let url = database_url.strip_prefix("sqlite:").unwrap_or(database_url);

    let manager = ConnectionManager::<SqliteConnection>::new(url);

    Pool::builder()
        .max_size(10)
        .connection_timeout(Duration::from_secs(30))
        .build(manager)
}

/// Initialize SQLite pragmas for a connection.
///
/// This should be called when a connection is first acquired from the pool.
pub fn init_connection_pragmas(conn: &mut SqliteConnection) -> Result<(), DieselError> {
    diesel::sql_query("PRAGMA journal_mode = WAL").execute(conn)?;
    diesel::sql_query("PRAGMA synchronous = NORMAL").execute(conn)?;
    diesel::sql_query("PRAGMA foreign_keys = ON").execute(conn)?;
    diesel::sql_query("PRAGMA cache_size = -64000").execute(conn)?; // 64MB
    diesel::sql_query("PRAGMA mmap_size = 268435456").execute(conn)?; // 256MB
    diesel::sql_query("PRAGMA temp_store = MEMORY").execute(conn)?;
    Ok(())
}

/// Run a blocking Diesel operation asynchronously.
///
/// This wraps a sync closure in spawn_blocking, allowing Diesel operations
/// to be used in async contexts without blocking the runtime.
///
/// # Example
/// ```ignore
/// let result = run_blocking(pool.clone(), |conn| {
///     sources::table.find("my-id").first::<SourceRecord>(conn)
/// }).await?;
/// ```
pub async fn run_blocking<F, T>(pool: SqlitePool, f: F) -> Result<T, DieselError>
where
    F: FnOnce(&mut SqliteConnection) -> Result<T, DieselError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut conn = pool.get().map_err(|e| {
            DieselError::DatabaseError(
                diesel::result::DatabaseErrorKind::Unknown,
                Box::new(e.to_string()),
            )
        })?;
        f(&mut conn)
    })
    .await
    .map_err(|e| {
        DieselError::DatabaseError(
            diesel::result::DatabaseErrorKind::Unknown,
            Box::new(e.to_string()),
        )
    })?
}
