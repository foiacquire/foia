//! SQLite-backed rate limiter for multi-process coordination.
//!
//! This module contains both sync (rusqlite) and async (sqlx) implementations.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use sqlx::sqlite::SqlitePool;

use super::rate_limit_backend::{
    DomainRateState, RateLimitBackend, RateLimitError, RateLimitResult,
};

/// SQLite-backed rate limit storage.
/// Uses file locking for multi-process coordination.
pub struct SqliteRateLimitBackend {
    conn: Mutex<Connection>,
}

impl SqliteRateLimitBackend {
    /// Create a new SQLite rate limit backend.
    pub fn new(db_path: &Path) -> RateLimitResult<Self> {
        let conn =
            Connection::open(db_path).map_err(|e| RateLimitError::Database(e.to_string()))?;

        // Enable WAL mode for better concurrent access
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| RateLimitError::Database(e.to_string()))?;

        // Set busy timeout for lock contention
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| RateLimitError::Database(e.to_string()))?;

        let backend = Self {
            conn: Mutex::new(conn),
        };

        backend.init_tables()?;
        Ok(backend)
    }

    /// Initialize database tables.
    fn init_tables(&self) -> RateLimitResult<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS rate_limit_domains (
                domain TEXT PRIMARY KEY,
                current_delay_ms INTEGER NOT NULL,
                last_request_at INTEGER,
                consecutive_successes INTEGER NOT NULL DEFAULT 0,
                in_backoff INTEGER NOT NULL DEFAULT 0,
                total_requests INTEGER NOT NULL DEFAULT 0,
                rate_limit_hits INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS rate_limit_403s (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                domain TEXT NOT NULL,
                url TEXT NOT NULL,
                timestamp_ms INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_403s_domain_time
                ON rate_limit_403s(domain, timestamp_ms);
        "#,
        )
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(())
    }

    fn row_to_state(row: &rusqlite::Row) -> rusqlite::Result<DomainRateState> {
        Ok(DomainRateState {
            domain: row.get(0)?,
            current_delay_ms: row.get::<_, i64>(1)? as u64,
            last_request_at: row.get(2)?,
            consecutive_successes: row.get::<_, i32>(3)? as u32,
            in_backoff: row.get::<_, i32>(4)? != 0,
            total_requests: row.get::<_, i64>(5)? as u64,
            rate_limit_hits: row.get::<_, i64>(6)? as u64,
        })
    }
}

#[async_trait]
impl RateLimitBackend for SqliteRateLimitBackend {
    async fn get_or_create_domain(
        &self,
        domain: &str,
        base_delay_ms: u64,
    ) -> RateLimitResult<DomainRateState> {
        let conn = self.conn.lock().unwrap();

        // Try to get existing
        let existing: Option<DomainRateState> = conn
            .query_row(
                "SELECT domain, current_delay_ms, last_request_at, consecutive_successes,
                        in_backoff, total_requests, rate_limit_hits
                 FROM rate_limit_domains WHERE domain = ?",
                params![domain],
                Self::row_to_state,
            )
            .optional()
            .map_err(|e| RateLimitError::Database(e.to_string()))?;

        if let Some(state) = existing {
            return Ok(state);
        }

        // Create new
        conn.execute(
            "INSERT INTO rate_limit_domains (domain, current_delay_ms) VALUES (?, ?)",
            params![domain, base_delay_ms as i64],
        )
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(DomainRateState::new(domain.to_string(), base_delay_ms))
    }

    async fn update_domain(&self, state: &DomainRateState) -> RateLimitResult<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            r#"UPDATE rate_limit_domains SET
                current_delay_ms = ?,
                last_request_at = ?,
                consecutive_successes = ?,
                in_backoff = ?,
                total_requests = ?,
                rate_limit_hits = ?
               WHERE domain = ?"#,
            params![
                state.current_delay_ms as i64,
                state.last_request_at,
                state.consecutive_successes as i32,
                state.in_backoff as i32,
                state.total_requests as i64,
                state.rate_limit_hits as i64,
                state.domain,
            ],
        )
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(())
    }

    async fn acquire(&self, domain: &str, base_delay_ms: u64) -> RateLimitResult<Duration> {
        let conn = self.conn.lock().unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis();

        // Use BEGIN IMMEDIATE for write lock
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| RateLimitError::Database(e.to_string()))?;

        let result = (|| -> RateLimitResult<Duration> {
            // Get or create domain state
            let state: Option<DomainRateState> = conn
                .query_row(
                    "SELECT domain, current_delay_ms, last_request_at, consecutive_successes,
                            in_backoff, total_requests, rate_limit_hits
                     FROM rate_limit_domains WHERE domain = ?",
                    params![domain],
                    Self::row_to_state,
                )
                .optional()
                .map_err(|e| RateLimitError::Database(e.to_string()))?;

            let (wait_time, _delay_ms) = match state {
                Some(s) => {
                    let wait = s.time_until_ready();
                    (wait, s.current_delay_ms)
                }
                None => {
                    // Create new domain entry
                    conn.execute(
                        "INSERT INTO rate_limit_domains (domain, current_delay_ms) VALUES (?, ?)",
                        params![domain, base_delay_ms as i64],
                    )
                    .map_err(|e| RateLimitError::Database(e.to_string()))?;
                    (Duration::ZERO, base_delay_ms)
                }
            };

            // Update last_request_at and increment total_requests
            let request_time = now_ms + wait_time.as_millis() as i64;
            conn.execute(
                "UPDATE rate_limit_domains SET last_request_at = ?, total_requests = total_requests + 1 WHERE domain = ?",
                params![request_time, domain],
            )
            .map_err(|e| RateLimitError::Database(e.to_string()))?;

            Ok(wait_time)
        })();

        match result {
            Ok(wait_time) => {
                conn.execute("COMMIT", [])
                    .map_err(|e| RateLimitError::Database(e.to_string()))?;
                Ok(wait_time)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    async fn record_403(&self, domain: &str, url: &str) -> RateLimitResult<()> {
        let conn = self.conn.lock().unwrap();
        let now_ms = chrono::Utc::now().timestamp_millis();

        conn.execute(
            "INSERT INTO rate_limit_403s (domain, url, timestamp_ms) VALUES (?, ?, ?)",
            params![domain, url, now_ms],
        )
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(())
    }

    async fn get_403_count(&self, domain: &str, window_ms: u64) -> RateLimitResult<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - window_ms as i64;

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT url) FROM rate_limit_403s WHERE domain = ? AND timestamp_ms > ?",
                params![domain, cutoff_ms],
                |row| row.get(0),
            )
            .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(count as usize)
    }

    async fn clear_403s(&self, domain: &str) -> RateLimitResult<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "DELETE FROM rate_limit_403s WHERE domain = ?",
            params![domain],
        )
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(())
    }

    async fn cleanup_expired_403s(&self, window_ms: u64) -> RateLimitResult<u64> {
        let conn = self.conn.lock().unwrap();
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - window_ms as i64;

        let deleted = conn
            .execute(
                "DELETE FROM rate_limit_403s WHERE timestamp_ms < ?",
                params![cutoff_ms],
            )
            .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(deleted as u64)
    }
}

// ============================================================================
// ASYNC (sqlx) implementation - truly async version
// ============================================================================

/// Async SQLx-backed rate limit storage.
/// Uses SQLite pool for true async operations.
#[derive(Clone)]
pub struct AsyncSqliteRateLimitBackend {
    pool: SqlitePool,
}

impl AsyncSqliteRateLimitBackend {
    /// Create a new async SQLite rate limit backend.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RateLimitBackend for AsyncSqliteRateLimitBackend {
    async fn get_or_create_domain(
        &self,
        domain: &str,
        base_delay_ms: u64,
    ) -> RateLimitResult<DomainRateState> {
        // Try to get existing
        let row = sqlx::query!(
            r#"SELECT
                domain as "domain!",
                current_delay_ms as "current_delay_ms!",
                last_request_at,
                consecutive_successes as "consecutive_successes!",
                in_backoff as "in_backoff!",
                total_requests as "total_requests!",
                rate_limit_hits as "rate_limit_hits!"
               FROM rate_limit_domains WHERE domain = ?"#,
            domain
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        if let Some(row) = row {
            return Ok(DomainRateState {
                domain: row.domain,
                current_delay_ms: row.current_delay_ms as u64,
                last_request_at: row.last_request_at,
                consecutive_successes: row.consecutive_successes as u32,
                in_backoff: row.in_backoff != 0,
                total_requests: row.total_requests as u64,
                rate_limit_hits: row.rate_limit_hits as u64,
            });
        }

        // Create new
        let delay = base_delay_ms as i64;
        sqlx::query!(
            "INSERT INTO rate_limit_domains (domain, current_delay_ms) VALUES (?, ?)",
            domain,
            delay
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(DomainRateState::new(domain.to_string(), base_delay_ms))
    }

    async fn update_domain(&self, state: &DomainRateState) -> RateLimitResult<()> {
        let delay = state.current_delay_ms as i64;
        let successes = state.consecutive_successes as i32;
        let in_backoff = state.in_backoff as i32;
        let total = state.total_requests as i64;
        let hits = state.rate_limit_hits as i64;

        sqlx::query!(
            r#"UPDATE rate_limit_domains SET
                current_delay_ms = ?,
                last_request_at = ?,
                consecutive_successes = ?,
                in_backoff = ?,
                total_requests = ?,
                rate_limit_hits = ?
               WHERE domain = ?"#,
            delay,
            state.last_request_at,
            successes,
            in_backoff,
            total,
            hits,
            state.domain
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(())
    }

    async fn acquire(&self, domain: &str, base_delay_ms: u64) -> RateLimitResult<Duration> {
        let now_ms = chrono::Utc::now().timestamp_millis();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| RateLimitError::Database(e.to_string()))?;

        // Get or create domain state
        let row = sqlx::query!(
            r#"SELECT
                domain as "domain!",
                current_delay_ms as "current_delay_ms!",
                last_request_at,
                consecutive_successes as "consecutive_successes!",
                in_backoff as "in_backoff!",
                total_requests as "total_requests!",
                rate_limit_hits as "rate_limit_hits!"
               FROM rate_limit_domains WHERE domain = ?"#,
            domain
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        let wait_time = match row {
            Some(row) => {
                let state = DomainRateState {
                    domain: row.domain,
                    current_delay_ms: row.current_delay_ms as u64,
                    last_request_at: row.last_request_at,
                    consecutive_successes: row.consecutive_successes as u32,
                    in_backoff: row.in_backoff != 0,
                    total_requests: row.total_requests as u64,
                    rate_limit_hits: row.rate_limit_hits as u64,
                };
                state.time_until_ready()
            }
            None => {
                // Create new domain entry
                let delay = base_delay_ms as i64;
                sqlx::query!(
                    "INSERT INTO rate_limit_domains (domain, current_delay_ms) VALUES (?, ?)",
                    domain,
                    delay
                )
                .execute(&mut *tx)
                .await
                .map_err(|e| RateLimitError::Database(e.to_string()))?;
                Duration::ZERO
            }
        };

        // Update last_request_at and increment total_requests
        let request_time = now_ms + wait_time.as_millis() as i64;
        sqlx::query!(
            "UPDATE rate_limit_domains SET last_request_at = ?, total_requests = total_requests + 1 WHERE domain = ?",
            request_time,
            domain
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(wait_time)
    }

    async fn record_403(&self, domain: &str, url: &str) -> RateLimitResult<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();

        sqlx::query!(
            "INSERT INTO rate_limit_403s (domain, url, timestamp_ms) VALUES (?, ?, ?)",
            domain,
            url,
            now_ms
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(())
    }

    async fn get_403_count(&self, domain: &str, window_ms: u64) -> RateLimitResult<usize> {
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - window_ms as i64;

        let count: i32 = sqlx::query_scalar!(
            r#"SELECT COUNT(DISTINCT url) as "count!: i32" FROM rate_limit_403s WHERE domain = ? AND timestamp_ms > ?"#,
            domain,
            cutoff_ms
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(count as usize)
    }

    async fn clear_403s(&self, domain: &str) -> RateLimitResult<()> {
        sqlx::query!("DELETE FROM rate_limit_403s WHERE domain = ?", domain)
            .execute(&self.pool)
            .await
            .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(())
    }

    async fn cleanup_expired_403s(&self, window_ms: u64) -> RateLimitResult<u64> {
        let cutoff_ms = chrono::Utc::now().timestamp_millis() - window_ms as i64;

        let result = sqlx::query!(
            "DELETE FROM rate_limit_403s WHERE timestamp_ms < ?",
            cutoff_ms
        )
        .execute(&self.pool)
        .await
        .map_err(|e| RateLimitError::Database(e.to_string()))?;

        Ok(result.rows_affected())
    }
}
