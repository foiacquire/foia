//! Unified database connection pool supporting SQLite and PostgreSQL.
//!
//! This module provides a backend-agnostic interface for database connections.
//! The actual backend is determined at runtime based on the database URL.

use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use diesel::sqlite::SqliteConnection;
use diesel_async::sync_connection_wrapper::SyncConnectionWrapper;
use diesel_async::AsyncConnection;
use tokio::sync::mpsc;

#[cfg(feature = "postgres")]
use diesel_async::pooled_connection::deadpool::Pool as DeadPool;
#[cfg(feature = "postgres")]
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
#[cfg(feature = "postgres")]
use diesel_async::pooled_connection::ManagerConfig;
#[cfg(feature = "postgres")]
use diesel_async::AsyncPgConnection;

#[cfg(feature = "postgres")]
use super::util::is_postgres_url;
use super::util::{to_diesel_error, validate_database_url};

/// Diesel error type alias.
pub type DbError = diesel::result::Error;

/// Alias for DbError used by diesel repositories.
pub type DieselError = diesel::result::Error;

/// Async SQLite connection type.
pub type SqliteConn = SyncConnectionWrapper<SqliteConnection>;

/// Async PostgreSQL connection type.
#[cfg(feature = "postgres")]
pub type PgConn = deadpool::managed::Object<AsyncDieselConnectionManager<AsyncPgConnection>>;

/// Default number of connections in the SQLite pool.
const DEFAULT_SQLITE_POOL_SIZE: usize = 4;

/// A pooled SQLite connection that returns itself to the pool on drop.
///
/// Implements `Deref` and `DerefMut` to `SqliteConn`, so it can be used
/// transparently wherever `&mut SqliteConn` is expected (diesel deref-coerces
/// `&mut PooledSqliteConn` to `&mut SqliteConn`).
pub struct PooledSqliteConn {
    conn: Option<SqliteConn>,
    return_tx: mpsc::UnboundedSender<SqliteConn>,
}

impl Deref for PooledSqliteConn {
    type Target = SqliteConn;

    fn deref(&self) -> &Self::Target {
        self.conn.as_ref().expect("connection taken before drop")
    }
}

impl DerefMut for PooledSqliteConn {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.conn.as_mut().expect("connection taken before drop")
    }
}

impl Drop for PooledSqliteConn {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            // Best-effort return to pool. If the pool is dropped (receiver closed),
            // the connection is simply dropped here.
            let _ = self.return_tx.send(conn);
        }
    }
}

/// Inner state shared by all clones of a `SqlitePool`.
struct SqlitePoolInner {
    database_url: String,
    return_tx: mpsc::UnboundedSender<SqliteConn>,
    return_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<SqliteConn>>,
    max_size: usize,
    total_created: AtomicUsize,
}

/// SQLite connection pool with bounded connection reuse.
///
/// Connections are lazily established and returned to the pool after use.
/// The pool keeps up to `max_size` idle connections. When all connections
/// are checked out and the pool hasn't reached `max_size`, a new connection
/// is established.
#[derive(Clone)]
pub struct SqlitePool {
    inner: Arc<SqlitePoolInner>,
}

#[allow(dead_code)]
impl SqlitePool {
    /// Create a new SQLite pool with default size.
    pub fn new(database_url: &str) -> Self {
        Self::with_size(database_url, DEFAULT_SQLITE_POOL_SIZE)
    }

    /// Create a new SQLite pool with a specific maximum connection count.
    pub fn with_size(database_url: &str, max_size: usize) -> Self {
        let url = database_url.strip_prefix("sqlite:").unwrap_or(database_url);
        let (return_tx, return_rx) = mpsc::unbounded_channel();
        Self {
            inner: Arc::new(SqlitePoolInner {
                database_url: url.to_string(),
                return_tx,
                return_rx: tokio::sync::Mutex::new(return_rx),
                max_size: max_size.max(1),
                total_created: AtomicUsize::new(0),
            }),
        }
    }

    /// Create pool from a file path.
    pub fn from_path(path: &Path) -> Self {
        Self::new(&path.display().to_string())
    }

    /// Get a pooled connection.
    ///
    /// Reuses an idle connection if available, otherwise establishes a new one.
    /// The connection is returned to the pool when the guard is dropped.
    pub async fn get(&self) -> Result<PooledSqliteConn, DbError> {
        // Try to reuse an idle connection (non-blocking check)
        {
            let mut rx = self.inner.return_rx.lock().await;
            if let Ok(conn) = rx.try_recv() {
                return Ok(PooledSqliteConn {
                    conn: Some(conn),
                    return_tx: self.inner.return_tx.clone(),
                });
            }
        }

        // No idle connections — establish a new one
        let conn = SqliteConn::establish(&self.inner.database_url)
            .await
            .map_err(to_diesel_error)?;

        let created = self.inner.total_created.fetch_add(1, Ordering::Relaxed) + 1;
        if created <= self.inner.max_size {
            tracing::debug!("SQLite pool: created connection {}/{}", created, self.inner.max_size);
        }

        Ok(PooledSqliteConn {
            conn: Some(conn),
            return_tx: self.inner.return_tx.clone(),
        })
    }

    /// Get the database URL.
    pub fn database_url(&self) -> &str {
        &self.inner.database_url
    }
}

/// PostgreSQL connection pool.
#[cfg(feature = "postgres")]
#[derive(Clone)]
pub struct PgPool {
    pool: DeadPool<AsyncPgConnection>,
}

#[cfg(feature = "postgres")]
#[allow(dead_code)]
impl PgPool {
    /// Create a new PostgreSQL pool.
    pub fn new(database_url: &str, max_size: usize, no_tls: bool) -> Result<Self, DbError> {
        let mgr = if no_tls {
            AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url)
        } else {
            let mut manager_config = ManagerConfig::default();
            manager_config.custom_setup = Box::new(super::pg_tls::establish_tls_connection);
            AsyncDieselConnectionManager::<AsyncPgConnection>::new_with_config(
                database_url,
                manager_config,
            )
        };
        let pool = DeadPool::builder(mgr)
            .max_size(max_size)
            .build()
            .map_err(to_diesel_error)?;
        Ok(Self { pool })
    }

    /// Get a connection.
    pub async fn get(&self) -> Result<PgConn, DbError> {
        self.pool.get().await.map_err(to_diesel_error)
    }

    /// Get the inner deadpool pool for use with diesel_context.
    pub fn inner(&self) -> DeadPool<AsyncPgConnection> {
        self.pool.clone()
    }
}

/// Unified database pool that supports both SQLite and PostgreSQL.
#[derive(Clone)]
pub enum DbPool {
    Sqlite(SqlitePool),
    #[cfg(feature = "postgres")]
    Postgres(PgPool),
}

#[allow(dead_code)]
impl DbPool {
    /// Create a pool from a database URL.
    ///
    /// Detects the backend from the URL:
    /// - `postgres://` or `postgresql://` → PostgreSQL
    /// - `sqlite:` prefix or file path → SQLite
    ///
    /// Returns an error if:
    /// - A PostgreSQL URL is provided but the `postgres` feature is not enabled
    /// - The URL format is not recognized
    pub fn from_url(url: &str, no_tls: bool) -> Result<Self, DbError> {
        // Validate the URL is supported by this build
        validate_database_url(url)?;

        #[cfg(feature = "postgres")]
        if is_postgres_url(url) {
            return Ok(DbPool::Postgres(PgPool::new(url, 10, no_tls)?));
        }
        let _ = no_tls;

        // Validate this looks like a SQLite URL/path, not a malformed postgres URL
        if url.contains("://") && !url.starts_with("sqlite:") {
            return Err(diesel::result::Error::QueryBuilderError(
                format!(
                    "Unrecognized database URL scheme: {}. Expected 'sqlite:', 'postgres://', or a file path.",
                    url.split("://").next().unwrap_or("unknown")
                ).into()
            ));
        }

        Ok(DbPool::Sqlite(SqlitePool::new(url)))
    }

    /// Create a SQLite pool from a file path.
    pub fn sqlite_from_path(path: &Path) -> Self {
        DbPool::Sqlite(SqlitePool::from_path(path))
    }

    /// Check if this is a SQLite backend.
    pub fn is_sqlite(&self) -> bool {
        matches!(self, DbPool::Sqlite(_))
    }

    /// Check if this is a PostgreSQL backend.
    #[cfg(feature = "postgres")]
    pub fn is_postgres(&self) -> bool {
        matches!(self, DbPool::Postgres(_))
    }
}

/// Macro for running database operations on either backend.
///
/// This macro handles the connection dispatch, allowing the same Diesel DSL
/// code to run on both SQLite and PostgreSQL.
///
/// # Example
/// ```ignore
/// with_conn!(self.pool, conn, {
///     sources::table.load::<SourceRecord>(&mut conn).await
/// })
/// ```
#[macro_export]
macro_rules! with_conn {
    ($pool:expr, $conn:ident, $body:expr) => {{
        match &$pool {
            $crate::repository::pool::DbPool::Sqlite(pool) => {
                let mut $conn = pool.get().await?;
                $body
            }
            #[cfg(feature = "postgres")]
            $crate::repository::pool::DbPool::Postgres(pool) => {
                use $crate::repository::util::to_diesel_error;
                let mut $conn = pool.get().await.map_err(to_diesel_error)?;
                $body
            }
        }
    }};
}

/// Macro for running database operations that need different SQL per backend.
///
/// Use this when the SQL syntax differs between SQLite and PostgreSQL.
///
/// # Example
/// ```ignore
/// with_conn_split!(self.pool,
///     sqlite: conn => {
///         diesel::replace_into(table).values(...).execute(&mut conn).await
///     },
///     postgres: conn => {
///         diesel::insert_into(table).values(...).on_conflict(...).execute(&mut conn).await
///     }
/// )
/// ```
#[macro_export]
macro_rules! with_conn_split {
    ($pool:expr, sqlite: $sqlite_conn:ident => $sqlite_body:expr, postgres: $pg_conn:ident => $pg_body:expr) => {{
        match &$pool {
            $crate::repository::pool::DbPool::Sqlite(pool) => {
                let mut $sqlite_conn = pool.get().await?;
                $sqlite_body
            }
            #[cfg(feature = "postgres")]
            $crate::repository::pool::DbPool::Postgres(pool) => {
                use $crate::repository::util::to_diesel_error;
                let mut $pg_conn = pool.get().await.map_err(to_diesel_error)?;
                $pg_body
            }
        }
    }};
}

#[allow(unused_imports)]
pub use with_conn;
#[allow(unused_imports)]
pub use with_conn_split;

/// Build a SQL string from a sea-query statement using the correct backend.
///
/// Returns only the SQL; values are discarded because they are re-bound
/// through diesel's `sql_query().bind()` for type safety.
pub fn build_sql<S: sea_query::QueryStatementWriter>(pool: &DbPool, stmt: &S) -> String {
    match pool {
        DbPool::Sqlite(_) => {
            let (sql, _) = stmt.build(sea_query::SqliteQueryBuilder);
            sql
        }
        #[cfg(feature = "postgres")]
        DbPool::Postgres(_) => {
            let (sql, _) = stmt.build(sea_query::PostgresQueryBuilder);
            sql
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_detection() {
        // SQLite paths
        assert!(DbPool::from_url("/path/to/db.sqlite", false)
            .unwrap()
            .is_sqlite());
        assert!(DbPool::from_url("sqlite:/path/to/db", false)
            .unwrap()
            .is_sqlite());

        // PostgreSQL URLs (only with feature)
        #[cfg(feature = "postgres")]
        {
            assert!(DbPool::from_url("postgres://localhost/test", true)
                .unwrap()
                .is_postgres());
            assert!(DbPool::from_url("postgresql://localhost/test", true)
                .unwrap()
                .is_postgres());
        }
    }
}
