//! Crawl state repository for tracking URL discovery and request history.
//!
//! This module contains both sync (rusqlite) and async (sqlx) implementations.

#![allow(dead_code)]

mod claim;
mod helpers;
mod request;
mod state;
mod url;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqlitePool;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::Result;
use crate::models::{CrawlRequest, CrawlState, CrawlUrl, DiscoveryMethod, RequestStats, UrlStatus};

/// SQLite-backed repository for crawl state.
pub struct CrawlRepository {
    db_path: PathBuf,
}

impl CrawlRepository {
    /// Create a new crawl repository.
    pub fn new(db_path: &Path) -> Result<Self> {
        let repo = Self {
            db_path: db_path.to_path_buf(),
        };
        repo.init_schema()?;
        Ok(repo)
    }

    pub(crate) fn connect(&self) -> Result<Connection> {
        super::connect(&self.db_path)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.connect()?;
        conn.execute_batch(
            r#"
            -- URLs discovered during crawling
            CREATE TABLE IF NOT EXISTS crawl_urls (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                url TEXT NOT NULL,
                source_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'discovered',

                -- Discovery context
                discovery_method TEXT NOT NULL DEFAULT 'seed',
                parent_url TEXT,
                discovery_context TEXT NOT NULL DEFAULT '{}',
                depth INTEGER NOT NULL DEFAULT 0,

                -- Timing
                discovered_at TEXT NOT NULL,
                fetched_at TEXT,

                -- Retry tracking
                retry_count INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                next_retry_at TEXT,

                -- HTTP caching
                etag TEXT,
                last_modified TEXT,

                -- Content linkage
                content_hash TEXT,
                document_id TEXT,

                UNIQUE(source_id, url)
            );

            -- HTTP request audit log
            CREATE TABLE IF NOT EXISTS crawl_requests (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_id TEXT NOT NULL,
                url TEXT NOT NULL,
                method TEXT NOT NULL DEFAULT 'GET',

                -- Request
                request_headers TEXT NOT NULL DEFAULT '{}',
                request_at TEXT NOT NULL,

                -- Response
                response_status INTEGER,
                response_headers TEXT NOT NULL DEFAULT '{}',
                response_at TEXT,
                response_size INTEGER,

                -- Timing
                duration_ms INTEGER,

                -- Error
                error TEXT,

                -- Conditional request tracking
                was_conditional INTEGER NOT NULL DEFAULT 0,
                was_not_modified INTEGER NOT NULL DEFAULT 0
            );

            -- Config hash tracking to detect when scraper config changes
            CREATE TABLE IF NOT EXISTS crawl_config (
                source_id TEXT PRIMARY KEY,
                config_hash TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- Indexes for efficient queries
            CREATE INDEX IF NOT EXISTS idx_crawl_urls_source_status
                ON crawl_urls(source_id, status);
            CREATE INDEX IF NOT EXISTS idx_crawl_urls_parent
                ON crawl_urls(parent_url);
            CREATE INDEX IF NOT EXISTS idx_crawl_urls_discovered
                ON crawl_urls(discovered_at);
            CREATE INDEX IF NOT EXISTS idx_crawl_urls_retry
                ON crawl_urls(next_retry_at) WHERE status = 'failed';
            CREATE INDEX IF NOT EXISTS idx_crawl_requests_source
                ON crawl_requests(source_id, request_at);
            CREATE INDEX IF NOT EXISTS idx_crawl_requests_url
                ON crawl_requests(url);
        "#,
        )?;
        Ok(())
    }

    /// Check if the scraper config has changed since last crawl.
    /// Returns (has_changed, should_clear) - should_clear is true if there are pending URLs.
    pub fn check_config_changed(
        &self,
        source_id: &str,
        config: &impl serde::Serialize,
    ) -> Result<(bool, bool)> {
        let conn = self.connect()?;

        // Compute hash of current config
        let config_json = serde_json::to_string(config).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(config_json.as_bytes());
        let current_hash = hex::encode(hasher.finalize());

        // Get stored hash
        let stored_hash: Option<String> = conn
            .query_row(
                "SELECT config_hash FROM crawl_config WHERE source_id = ?",
                params![source_id],
                |row| row.get(0),
            )
            .ok();

        let has_changed = stored_hash.as_ref() != Some(&current_hash);

        // Check if there are pending URLs that would be affected
        let pending_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM crawl_urls WHERE source_id = ? AND status IN ('discovered', 'fetching')",
                params![source_id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        Ok((has_changed, has_changed && pending_count > 0))
    }

    /// Store the current config hash for a source.
    pub fn store_config_hash(&self, source_id: &str, config: &impl serde::Serialize) -> Result<()> {
        let conn = self.connect()?;

        let config_json = serde_json::to_string(config).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(config_json.as_bytes());
        let config_hash = hex::encode(hasher.finalize());

        conn.execute(
            "INSERT OR REPLACE INTO crawl_config (source_id, config_hash, updated_at) VALUES (?, ?, ?)",
            params![source_id, config_hash, Utc::now().to_rfc3339()],
        )?;

        Ok(())
    }
}

// ============================================================================
// ASYNC (sqlx) implementation - for new code and gradual migration
// ============================================================================

/// Row type for CrawlUrl SQLx query mapping.
#[derive(sqlx::FromRow)]
struct CrawlUrlRow {
    #[allow(dead_code)]
    id: i64,
    url: String,
    source_id: String,
    status: String,
    discovery_method: String,
    parent_url: Option<String>,
    discovery_context: String,
    depth: i64,
    discovered_at: String,
    fetched_at: Option<String>,
    retry_count: i64,
    last_error: Option<String>,
    next_retry_at: Option<String>,
    etag: Option<String>,
    last_modified: Option<String>,
    content_hash: Option<String>,
    document_id: Option<String>,
}

impl From<CrawlUrlRow> for CrawlUrl {
    fn from(row: CrawlUrlRow) -> Self {
        let discovery_context: HashMap<String, serde_json::Value> =
            serde_json::from_str(&row.discovery_context).unwrap_or_default();

        CrawlUrl {
            url: row.url,
            source_id: row.source_id,
            status: UrlStatus::from_str(&row.status).unwrap_or(UrlStatus::Discovered),
            discovery_method: DiscoveryMethod::from_str(&row.discovery_method)
                .unwrap_or(DiscoveryMethod::Seed),
            parent_url: row.parent_url,
            discovery_context,
            depth: row.depth as u32,
            discovered_at: DateTime::parse_from_rfc3339(&row.discovered_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            fetched_at: row
                .fetched_at
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&Utc)),
            retry_count: row.retry_count as u32,
            last_error: row.last_error,
            next_retry_at: row
                .next_retry_at
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&Utc)),
            etag: row.etag,
            last_modified: row.last_modified,
            content_hash: row.content_hash,
            document_id: row.document_id,
        }
    }
}

/// Row type for CrawlRequest SQLx query mapping.
#[derive(sqlx::FromRow)]
struct CrawlRequestRow {
    id: i64,
    source_id: String,
    url: String,
    method: String,
    request_headers: String,
    request_at: String,
    response_status: Option<i64>,
    response_headers: String,
    response_at: Option<String>,
    response_size: Option<i64>,
    duration_ms: Option<i64>,
    error: Option<String>,
    was_conditional: i64,
    was_not_modified: i64,
}

impl From<CrawlRequestRow> for CrawlRequest {
    fn from(row: CrawlRequestRow) -> Self {
        CrawlRequest {
            id: Some(row.id),
            source_id: row.source_id,
            url: row.url,
            method: row.method,
            request_headers: serde_json::from_str(&row.request_headers).unwrap_or_default(),
            request_at: DateTime::parse_from_rfc3339(&row.request_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            response_status: row.response_status.map(|s| s as u16),
            response_headers: serde_json::from_str(&row.response_headers).unwrap_or_default(),
            response_at: row
                .response_at
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&Utc)),
            response_size: row.response_size.map(|s| s as u64),
            duration_ms: row.duration_ms.map(|d| d as u64),
            error: row.error,
            was_conditional: row.was_conditional != 0,
            was_not_modified: row.was_not_modified != 0,
        }
    }
}

/// Async SQLx-backed repository for crawl state.
#[derive(Clone)]
pub struct AsyncCrawlRepository {
    pool: SqlitePool,
}

impl AsyncCrawlRepository {
    /// Create a new async crawl repository with an existing pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Check if the scraper config has changed since last crawl.
    /// Returns (has_changed, should_clear) - should_clear is true if there are pending URLs.
    pub async fn check_config_changed(
        &self,
        source_id: &str,
        config: &impl serde::Serialize,
    ) -> Result<(bool, bool)> {
        // Compute hash of current config
        let config_json = serde_json::to_string(config).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(config_json.as_bytes());
        let current_hash = hex::encode(hasher.finalize());

        // Get stored hash
        let stored_hash: Option<String> = sqlx::query_scalar!(
            r#"SELECT config_hash as "config_hash!" FROM crawl_config WHERE source_id = ?"#,
            source_id
        )
        .fetch_optional(&self.pool)
        .await?;

        let has_changed = stored_hash.as_ref() != Some(&current_hash);

        // Check if there are pending URLs that would be affected
        let pending_count: i32 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "count!: i32" FROM crawl_urls
               WHERE source_id = ? AND status IN ('discovered', 'fetching')"#,
            source_id
        )
        .fetch_one(&self.pool)
        .await?;

        Ok((has_changed, has_changed && pending_count > 0))
    }

    /// Store the current config hash for a source.
    pub async fn store_config_hash(
        &self,
        source_id: &str,
        config: &impl serde::Serialize,
    ) -> Result<()> {
        let config_json = serde_json::to_string(config).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(config_json.as_bytes());
        let config_hash = hex::encode(hasher.finalize());
        let now = Utc::now().to_rfc3339();

        sqlx::query!(
            r#"INSERT OR REPLACE INTO crawl_config (source_id, config_hash, updated_at)
               VALUES (?, ?, ?)"#,
            source_id,
            config_hash,
            now
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // ========================================================================
    // URL CRUD operations (from url.rs)
    // ========================================================================

    /// Add a discovered URL if not already known.
    pub async fn add_url(&self, crawl_url: &CrawlUrl) -> Result<bool> {
        let status = crawl_url.status.as_str();
        let discovery_method = crawl_url.discovery_method.as_str();
        let discovery_context = serde_json::to_string(&crawl_url.discovery_context)?;
        let depth = crawl_url.depth as i32;
        let discovered_at = crawl_url.discovered_at.to_rfc3339();
        let retry_count = crawl_url.retry_count as i32;

        let result = sqlx::query!(
            r#"INSERT OR IGNORE INTO crawl_urls (
                url, source_id, status, discovery_method, parent_url,
                discovery_context, depth, discovered_at, retry_count
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            crawl_url.url,
            crawl_url.source_id,
            status,
            discovery_method,
            crawl_url.parent_url,
            discovery_context,
            depth,
            discovered_at,
            retry_count
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Get a specific URL's crawl state.
    pub async fn get_url(&self, source_id: &str, url: &str) -> Result<Option<CrawlUrl>> {
        let row = sqlx::query_as!(
            CrawlUrlRow,
            r#"SELECT
                id as "id!",
                url as "url!",
                source_id as "source_id!",
                status as "status!",
                discovery_method as "discovery_method!",
                parent_url,
                discovery_context as "discovery_context!",
                depth as "depth!",
                discovered_at as "discovered_at!",
                fetched_at,
                retry_count as "retry_count!",
                last_error,
                next_retry_at,
                etag,
                last_modified,
                content_hash,
                document_id
               FROM crawl_urls WHERE source_id = ? AND url = ?"#,
            source_id,
            url
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(CrawlUrl::from))
    }

    /// Check if a URL has already been discovered.
    pub async fn url_exists(&self, source_id: &str, url: &str) -> Result<bool> {
        let count: i32 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "count!: i32" FROM crawl_urls
               WHERE source_id = ? AND url = ?"#,
            source_id,
            url
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count > 0)
    }

    /// Update an existing URL's state.
    pub async fn update_url(&self, crawl_url: &CrawlUrl) -> Result<()> {
        let status = crawl_url.status.as_str();
        let fetched_at = crawl_url.fetched_at.map(|dt| dt.to_rfc3339());
        let retry_count = crawl_url.retry_count as i32;
        let next_retry_at = crawl_url.next_retry_at.map(|dt| dt.to_rfc3339());

        sqlx::query!(
            r#"UPDATE crawl_urls SET
                status = ?1,
                fetched_at = ?2,
                retry_count = ?3,
                last_error = ?4,
                next_retry_at = ?5,
                etag = ?6,
                last_modified = ?7,
                content_hash = ?8,
                document_id = ?9
            WHERE source_id = ?10 AND url = ?11"#,
            status,
            fetched_at,
            retry_count,
            crawl_url.last_error,
            next_retry_at,
            crawl_url.etag,
            crawl_url.last_modified,
            crawl_url.content_hash,
            crawl_url.document_id,
            crawl_url.source_id,
            crawl_url.url
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Mark a URL for refresh by changing its status back to 'discovered'.
    pub async fn mark_url_for_refresh(&self, source_id: &str, url: &str) -> Result<()> {
        sqlx::query!(
            r#"UPDATE crawl_urls SET status = 'discovered' WHERE source_id = ? AND url = ?"#,
            source_id,
            url
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Get URLs that haven't been checked since a given time.
    pub async fn get_urls_needing_refresh(
        &self,
        source_id: &str,
        older_than: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<CrawlUrl>> {
        let older_than_str = older_than.to_rfc3339();
        let limit = limit as i32;

        let rows = sqlx::query_as!(
            CrawlUrlRow,
            r#"SELECT
                id as "id!",
                url as "url!",
                source_id as "source_id!",
                status as "status!",
                discovery_method as "discovery_method!",
                parent_url,
                discovery_context as "discovery_context!",
                depth as "depth!",
                discovered_at as "discovered_at!",
                fetched_at,
                retry_count as "retry_count!",
                last_error,
                next_retry_at,
                etag,
                last_modified,
                content_hash,
                document_id
               FROM crawl_urls
               WHERE source_id = ?
               AND status = 'fetched'
               AND fetched_at < ?
               ORDER BY fetched_at ASC
               LIMIT ?"#,
            source_id,
            older_than_str,
            limit
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(CrawlUrl::from).collect())
    }

    /// Get recently fetched URLs (successfully completed).
    pub async fn get_recent_downloads(
        &self,
        source_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<CrawlUrl>> {
        let limit = limit as i32;

        let rows = sqlx::query_as!(
            CrawlUrlRow,
            r#"SELECT
                id as "id!",
                url as "url!",
                source_id as "source_id!",
                status as "status!",
                discovery_method as "discovery_method!",
                parent_url,
                discovery_context as "discovery_context!",
                depth as "depth!",
                discovered_at as "discovered_at!",
                fetched_at,
                retry_count as "retry_count!",
                last_error,
                next_retry_at,
                etag,
                last_modified,
                content_hash,
                document_id
               FROM crawl_urls
               WHERE (?1 IS NULL OR source_id = ?1) AND status = 'fetched'
               ORDER BY fetched_at DESC
               LIMIT ?2"#,
            source_id,
            limit
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(CrawlUrl::from).collect())
    }

    /// Get failed URLs with their error messages.
    pub async fn get_failed_urls(
        &self,
        source_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<CrawlUrl>> {
        let limit = limit as i32;

        let rows = sqlx::query_as!(
            CrawlUrlRow,
            r#"SELECT
                id as "id!",
                url as "url!",
                source_id as "source_id!",
                status as "status!",
                discovery_method as "discovery_method!",
                parent_url,
                discovery_context as "discovery_context!",
                depth as "depth!",
                discovered_at as "discovered_at!",
                fetched_at,
                retry_count as "retry_count!",
                last_error,
                next_retry_at,
                etag,
                last_modified,
                content_hash,
                document_id
               FROM crawl_urls
               WHERE (?1 IS NULL OR source_id = ?1) AND status IN ('failed', 'exhausted')
               ORDER BY fetched_at DESC NULLS LAST
               LIMIT ?2"#,
            source_id,
            limit
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(CrawlUrl::from).collect())
    }

    /// Clear pending crawl state for a source (keeps fetched URLs).
    pub async fn clear_source(&self, source_id: &str) -> Result<()> {
        sqlx::query!(
            r#"DELETE FROM crawl_urls
               WHERE source_id = ? AND status IN ('discovered', 'fetching', 'failed')"#,
            source_id
        )
        .execute(&self.pool)
        .await?;

        sqlx::query!("DELETE FROM crawl_requests WHERE source_id = ?", source_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Clear ALL crawl state for a source (including fetched URLs).
    pub async fn clear_source_all(&self, source_id: &str) -> Result<()> {
        sqlx::query!("DELETE FROM crawl_urls WHERE source_id = ?", source_id)
            .execute(&self.pool)
            .await?;

        sqlx::query!("DELETE FROM crawl_requests WHERE source_id = ?", source_id)
            .execute(&self.pool)
            .await?;

        sqlx::query!("DELETE FROM crawl_config WHERE source_id = ?", source_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    // ========================================================================
    // Request logging operations (from request.rs)
    // ========================================================================

    /// Log an HTTP request and return its ID.
    pub async fn log_request(&self, request: &CrawlRequest) -> Result<i64> {
        let request_headers = serde_json::to_string(&request.request_headers)?;
        let request_at = request.request_at.to_rfc3339();
        let response_status = request.response_status.map(|s| s as i32);
        let response_headers = serde_json::to_string(&request.response_headers)?;
        let response_at = request.response_at.map(|dt| dt.to_rfc3339());
        let response_size = request.response_size.map(|s| s as i64);
        let duration_ms = request.duration_ms.map(|d| d as i64);
        let was_conditional = request.was_conditional as i32;
        let was_not_modified = request.was_not_modified as i32;

        let result = sqlx::query!(
            r#"INSERT INTO crawl_requests (
                source_id, url, method, request_headers, request_at,
                response_status, response_headers, response_at,
                response_size, duration_ms, error,
                was_conditional, was_not_modified
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"#,
            request.source_id,
            request.url,
            request.method,
            request_headers,
            request_at,
            response_status,
            response_headers,
            response_at,
            response_size,
            duration_ms,
            request.error,
            was_conditional,
            was_not_modified
        )
        .execute(&self.pool)
        .await?;

        Ok(result.last_insert_rowid())
    }

    /// Get the most recent request for a URL.
    pub async fn get_last_request(
        &self,
        source_id: &str,
        url: &str,
    ) -> Result<Option<CrawlRequest>> {
        let row = sqlx::query_as!(
            CrawlRequestRow,
            r#"SELECT
                id as "id!",
                source_id as "source_id!",
                url as "url!",
                method as "method!",
                request_headers as "request_headers!",
                request_at as "request_at!",
                response_status,
                response_headers as "response_headers!",
                response_at,
                response_size,
                duration_ms,
                error,
                was_conditional as "was_conditional!",
                was_not_modified as "was_not_modified!"
               FROM crawl_requests
               WHERE source_id = ? AND url = ?
               ORDER BY request_at DESC
               LIMIT 1"#,
            source_id,
            url
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(CrawlRequest::from))
    }

    // ========================================================================
    // Claim operations (from claim.rs)
    // ========================================================================

    /// Get URLs that need to be fetched.
    pub async fn get_pending_urls(&self, source_id: &str, limit: u32) -> Result<Vec<CrawlUrl>> {
        let limit = limit as i32;

        let rows = sqlx::query_as!(
            CrawlUrlRow,
            r#"SELECT
                id as "id!",
                url as "url!",
                source_id as "source_id!",
                status as "status!",
                discovery_method as "discovery_method!",
                parent_url,
                discovery_context as "discovery_context!",
                depth as "depth!",
                discovered_at as "discovered_at!",
                fetched_at,
                retry_count as "retry_count!",
                last_error,
                next_retry_at,
                etag,
                last_modified,
                content_hash,
                document_id
               FROM crawl_urls
               WHERE source_id = ?
               AND status IN ('discovered', 'fetching')
               ORDER BY depth ASC, discovered_at ASC
               LIMIT ?"#,
            source_id,
            limit
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(CrawlUrl::from).collect())
    }

    /// Atomically claim a pending URL for processing.
    pub async fn claim_pending_url(&self, source_id: Option<&str>) -> Result<Option<CrawlUrl>> {
        let mut tx = self.pool.begin().await?;

        // Find a pending URL
        let row = if let Some(sid) = source_id {
            sqlx::query_as!(
                CrawlUrlRow,
                r#"SELECT
                    id as "id!",
                    url as "url!",
                    source_id as "source_id!",
                    status as "status!",
                    discovery_method as "discovery_method!",
                    parent_url,
                    discovery_context as "discovery_context!",
                    depth as "depth!",
                    discovered_at as "discovered_at!",
                    fetched_at,
                    retry_count as "retry_count!",
                    last_error,
                    next_retry_at,
                    etag,
                    last_modified,
                    content_hash,
                    document_id
                   FROM crawl_urls
                   WHERE source_id = ? AND status = 'discovered'
                   ORDER BY depth ASC, discovered_at ASC
                   LIMIT 1"#,
                sid
            )
            .fetch_optional(&mut *tx)
            .await?
        } else {
            sqlx::query_as!(
                CrawlUrlRow,
                r#"SELECT
                    id as "id!",
                    url as "url!",
                    source_id as "source_id!",
                    status as "status!",
                    discovery_method as "discovery_method!",
                    parent_url,
                    discovery_context as "discovery_context!",
                    depth as "depth!",
                    discovered_at as "discovered_at!",
                    fetched_at,
                    retry_count as "retry_count!",
                    last_error,
                    next_retry_at,
                    etag,
                    last_modified,
                    content_hash,
                    document_id
                   FROM crawl_urls
                   WHERE status = 'discovered'
                   ORDER BY depth ASC, discovered_at ASC
                   LIMIT 1"#
            )
            .fetch_optional(&mut *tx)
            .await?
        };

        if let Some(row) = row {
            let mut crawl_url = CrawlUrl::from(row);

            // Mark as fetching
            sqlx::query!(
                "UPDATE crawl_urls SET status = 'fetching' WHERE source_id = ? AND url = ?",
                crawl_url.source_id,
                crawl_url.url
            )
            .execute(&mut *tx)
            .await?;

            crawl_url.status = UrlStatus::Fetching;
            tx.commit().await?;
            Ok(Some(crawl_url))
        } else {
            tx.commit().await?;
            Ok(None)
        }
    }

    /// Atomically claim multiple pending URLs for processing.
    pub async fn claim_pending_urls(
        &self,
        source_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<CrawlUrl>> {
        let mut tx = self.pool.begin().await?;
        let limit = limit as i32;

        // Find pending URLs
        let rows = sqlx::query_as!(
            CrawlUrlRow,
            r#"SELECT
                id as "id!",
                url as "url!",
                source_id as "source_id!",
                status as "status!",
                discovery_method as "discovery_method!",
                parent_url,
                discovery_context as "discovery_context!",
                depth as "depth!",
                discovered_at as "discovered_at!",
                fetched_at,
                retry_count as "retry_count!",
                last_error,
                next_retry_at,
                etag,
                last_modified,
                content_hash,
                document_id
               FROM crawl_urls
               WHERE (?1 IS NULL OR source_id = ?1) AND status = 'discovered'
               ORDER BY depth ASC, discovered_at ASC
               LIMIT ?2"#,
            source_id,
            limit
        )
        .fetch_all(&mut *tx)
        .await?;

        let mut urls: Vec<CrawlUrl> = Vec::with_capacity(rows.len());

        for row in rows {
            let mut crawl_url = CrawlUrl::from(row);

            // Mark as fetching
            sqlx::query!(
                "UPDATE crawl_urls SET status = 'fetching' WHERE source_id = ? AND url = ?",
                crawl_url.source_id,
                crawl_url.url
            )
            .execute(&mut *tx)
            .await?;

            crawl_url.status = UrlStatus::Fetching;
            urls.push(crawl_url);
        }

        tx.commit().await?;
        Ok(urls)
    }

    /// Get failed URLs that are ready for retry.
    pub async fn get_retryable_urls(&self, source_id: &str, limit: u32) -> Result<Vec<CrawlUrl>> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let exhausted_cutoff = (now - chrono::Duration::days(70)).to_rfc3339();
        let limit = limit as i32;

        let rows = sqlx::query_as!(
            CrawlUrlRow,
            r#"SELECT
                id as "id!",
                url as "url!",
                source_id as "source_id!",
                status as "status!",
                discovery_method as "discovery_method!",
                parent_url,
                discovery_context as "discovery_context!",
                depth as "depth!",
                discovered_at as "discovered_at!",
                fetched_at,
                retry_count as "retry_count!",
                last_error,
                next_retry_at,
                etag,
                last_modified,
                content_hash,
                document_id
               FROM crawl_urls
               WHERE source_id = ?
               AND (
                   (status = 'failed' AND (next_retry_at IS NULL OR next_retry_at <= ?))
                   OR (status = 'exhausted' AND (next_retry_at IS NULL OR next_retry_at < ?))
               )
               ORDER BY retry_count ASC, discovered_at ASC
               LIMIT ?"#,
            source_id,
            now_str,
            exhausted_cutoff,
            limit
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(CrawlUrl::from).collect())
    }

    // ========================================================================
    // State and statistics queries (from state.rs)
    // ========================================================================

    /// Get aggregate crawl state for a source.
    pub async fn get_crawl_state(&self, source_id: &str) -> Result<CrawlState> {
        // Query 1: Get status counts
        let status_rows = sqlx::query!(
            r#"SELECT status as "status!", COUNT(*) as "count!: i64"
               FROM crawl_urls WHERE source_id = ?
               GROUP BY status"#,
            source_id
        )
        .fetch_all(&self.pool)
        .await?;

        let mut status_counts: HashMap<String, u64> = HashMap::new();
        for row in status_rows {
            status_counts.insert(row.status, row.count as u64);
        }

        // Query 2: Get timing info
        let timing = sqlx::query!(
            r#"SELECT
                MIN(discovered_at) as "first_discovered: String",
                MAX(fetched_at) as "last_fetched: String",
                MIN(CASE WHEN status IN ('discovered', 'fetching')
                    THEN discovered_at END) as "oldest_pending: String"
               FROM crawl_urls WHERE source_id = ?"#,
            source_id
        )
        .fetch_one(&self.pool)
        .await?;

        // Query 3: Get unexplored branch count
        let unexplored_count: i32 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "count!: i32" FROM crawl_urls u1
               WHERE u1.source_id = ?
               AND u1.status = 'fetched'
               AND u1.discovery_method IN ('html_link', 'pagination', 'api_result')
               AND NOT EXISTS (
                   SELECT 1 FROM crawl_urls u2
                   WHERE u2.source_id = u1.source_id
                   AND u2.parent_url = u1.url
               )
               AND u1.depth < 10"#,
            source_id
        )
        .fetch_one(&self.pool)
        .await?;

        let urls_discovered: u64 = status_counts.values().sum();
        let urls_fetched = *status_counts.get("fetched").unwrap_or(&0);
        let urls_failed = status_counts.get("failed").unwrap_or(&0)
            + status_counts.get("exhausted").unwrap_or(&0);
        let urls_pending = status_counts.get("discovered").unwrap_or(&0)
            + status_counts.get("fetching").unwrap_or(&0);

        Ok(CrawlState {
            source_id: source_id.to_string(),
            last_crawl_started: timing
                .first_discovered
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&Utc)),
            last_crawl_completed: if urls_pending == 0 {
                timing
                    .last_fetched
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
            } else {
                None
            },
            urls_discovered,
            urls_fetched,
            urls_failed,
            urls_pending,
            has_pending_urls: urls_pending > 0,
            has_unexplored_branches: unexplored_count > 0,
            oldest_pending_url: timing
                .oldest_pending
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&Utc)),
        })
    }

    /// Count crawl URLs for a source.
    pub async fn count_by_source(&self, source_id: &str) -> Result<u64> {
        let count: i32 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "count!: i32" FROM crawl_urls WHERE source_id = ?"#,
            source_id
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count as u64)
    }

    /// Get request statistics for a source.
    pub async fn get_request_stats(&self, source_id: &str) -> Result<RequestStats> {
        let stats = sqlx::query!(
            r#"SELECT
                COUNT(*) as "total_requests!: i64",
                SUM(CASE WHEN response_status = 200 THEN 1 ELSE 0 END) as "success_200: i64",
                SUM(CASE WHEN response_status = 304 THEN 1 ELSE 0 END) as "not_modified_304: i64",
                SUM(CASE WHEN response_status >= 400 THEN 1 ELSE 0 END) as "errors: i64",
                SUM(was_conditional) as "conditional_requests: i64",
                AVG(duration_ms) as "avg_duration_ms: f64",
                SUM(response_size) as "total_bytes: i64"
               FROM crawl_requests
               WHERE source_id = ?"#,
            source_id
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(RequestStats {
            total_requests: stats.total_requests as u64,
            success_200: stats.success_200.unwrap_or(0) as u64,
            not_modified_304: stats.not_modified_304.unwrap_or(0) as u64,
            errors: stats.errors.unwrap_or(0) as u64,
            conditional_requests: stats.conditional_requests.unwrap_or(0) as u64,
            avg_duration_ms: stats.avg_duration_ms.unwrap_or(0.0),
            total_bytes: stats.total_bytes.unwrap_or(0) as u64,
        })
    }

    /// Get request statistics for all sources (bulk query).
    pub async fn get_all_request_stats(&self) -> Result<HashMap<String, RequestStats>> {
        let rows = sqlx::query!(
            r#"SELECT
                source_id as "source_id!",
                COUNT(*) as "total_requests!: i64",
                COALESCE(SUM(CASE WHEN response_status = 200 THEN 1 ELSE 0 END), 0) as "success_200!: i64",
                COALESCE(SUM(CASE WHEN response_status = 304 THEN 1 ELSE 0 END), 0) as "not_modified_304!: i64",
                COALESCE(SUM(CASE WHEN response_status >= 400 THEN 1 ELSE 0 END), 0) as "errors!: i64",
                COALESCE(SUM(was_conditional), 0) as "conditional_requests!: i64",
                COALESCE(AVG(duration_ms), 0.0) as "avg_duration_ms!: f64",
                COALESCE(SUM(response_size), 0) as "total_bytes!: i64"
               FROM crawl_requests
               GROUP BY source_id"#
        )
        .fetch_all(&self.pool)
        .await?;

        let mut stats = HashMap::new();
        for row in rows {
            stats.insert(
                row.source_id,
                RequestStats {
                    total_requests: row.total_requests as u64,
                    success_200: row.success_200 as u64,
                    not_modified_304: row.not_modified_304 as u64,
                    errors: row.errors as u64,
                    conditional_requests: row.conditional_requests as u64,
                    avg_duration_ms: row.avg_duration_ms,
                    total_bytes: row.total_bytes as u64,
                },
            );
        }

        Ok(stats)
    }

    /// Get aggregate stats across all sources (bulk query).
    pub async fn get_all_stats(&self) -> Result<HashMap<String, CrawlState>> {
        // Bulk query 1: Get all status counts grouped by source
        let status_rows = sqlx::query!(
            r#"SELECT
                source_id as "source_id!",
                status as "status!",
                COUNT(*) as "count!: i64"
               FROM crawl_urls
               GROUP BY source_id, status"#
        )
        .fetch_all(&self.pool)
        .await?;

        let mut status_by_source: HashMap<String, HashMap<String, u64>> = HashMap::new();
        for row in status_rows {
            status_by_source
                .entry(row.source_id)
                .or_default()
                .insert(row.status, row.count as u64);
        }

        // Bulk query 2: Get timing info for all sources
        let timing_rows = sqlx::query!(
            r#"SELECT
                source_id as "source_id!",
                MIN(discovered_at) as "first_discovered: String",
                MAX(fetched_at) as "last_fetched: String",
                MIN(CASE WHEN status IN ('discovered', 'fetching')
                    THEN discovered_at END) as "oldest_pending: String"
               FROM crawl_urls
               GROUP BY source_id"#
        )
        .fetch_all(&self.pool)
        .await?;

        #[allow(clippy::type_complexity)]
        let mut timing_by_source: HashMap<
            String,
            (Option<String>, Option<String>, Option<String>),
        > = HashMap::new();
        for row in timing_rows {
            timing_by_source.insert(
                row.source_id,
                (
                    Some(row.first_discovered),
                    row.last_fetched.map(|s| s.to_string()),
                    row.oldest_pending.map(|s| s.to_string()),
                ),
            );
        }

        // Bulk query 3: Get unexplored branch counts for all sources
        let unexplored_rows = sqlx::query!(
            r#"SELECT u1.source_id as "source_id!", COUNT(*) as "count!: i64"
               FROM crawl_urls u1
               WHERE u1.status = 'fetched'
               AND u1.discovery_method IN ('html_link', 'pagination', 'api_result')
               AND NOT EXISTS (
                   SELECT 1 FROM crawl_urls u2
                   WHERE u2.source_id = u1.source_id
                   AND u2.parent_url = u1.url
               )
               AND u1.depth < 10
               GROUP BY u1.source_id"#
        )
        .fetch_all(&self.pool)
        .await?;

        let mut unexplored_by_source: HashMap<String, i64> = HashMap::new();
        for row in unexplored_rows {
            unexplored_by_source.insert(row.source_id, row.count);
        }

        // Build CrawlState for each source
        let mut stats = HashMap::new();
        for (source_id, status_counts) in status_by_source {
            let timing = timing_by_source
                .get(&source_id)
                .cloned()
                .unwrap_or((None, None, None));

            let unexplored_count = unexplored_by_source.get(&source_id).copied().unwrap_or(0);

            let urls_discovered: u64 = status_counts.values().sum();
            let urls_fetched = *status_counts.get("fetched").unwrap_or(&0);
            let urls_failed = status_counts.get("failed").unwrap_or(&0)
                + status_counts.get("exhausted").unwrap_or(&0);
            let urls_pending = status_counts.get("discovered").unwrap_or(&0)
                + status_counts.get("fetching").unwrap_or(&0);

            let state = CrawlState {
                source_id: source_id.clone(),
                last_crawl_started: timing
                    .0
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|dt| dt.with_timezone(&Utc)),
                last_crawl_completed: if urls_pending == 0 {
                    timing
                        .1
                        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                        .map(|dt| dt.with_timezone(&Utc))
                } else {
                    None
                },
                urls_discovered,
                urls_fetched,
                urls_failed,
                urls_pending,
                has_pending_urls: urls_pending > 0,
                has_unexplored_branches: unexplored_count > 0,
                oldest_pending_url: timing
                    .2
                    .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                    .map(|dt| dt.with_timezone(&Utc)),
            };

            stats.insert(source_id, state);
        }

        Ok(stats)
    }
}
