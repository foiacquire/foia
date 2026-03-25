//! Database repository layer for FOIA document system.
//!
//! All database access uses Diesel ORM with compile-time query checking.
//! Supports both SQLite and PostgreSQL backends.

// Core modules
pub mod context;
pub mod models;
pub mod pool;
pub mod schema;
pub mod sea_tables;

// Repositories
pub mod source;
pub mod diesel_config_history;
pub mod diesel_crawl;
pub mod diesel_document;
pub mod diesel_scraper_config;
pub mod diesel_context;
pub mod diesel_service_status;
pub mod diesel_source;

// Utilities
pub mod util;

// Database migration
pub mod migration;
#[cfg(feature = "postgres")]
pub mod migration_postgres;
pub mod migration_sqlite;
#[cfg(feature = "postgres")]
pub mod pg_tls;

// Migration runner
pub mod migrate;

// Diesel migrations (sea-query based)
pub mod migrations;

// Document helpers (types like DocumentNavigation, etc.)
mod document;

// Re-export main types
pub use context::DbContext;
pub use pool::{DbError, DbPool, DieselError};
pub use source::SourceRepository;

// Repository re-exports
pub use diesel_config_history::DieselConfigHistoryRepository;
pub use diesel_crawl::DieselCrawlRepository;
pub use diesel_document::DieselDocumentRepository;
pub use diesel_scraper_config::DieselScraperConfigRepository;
pub use diesel_service_status::DieselServiceStatusRepository;
pub use diesel_source::DieselSourceRepository;
pub use migration::{DatabaseExporter, DatabaseImporter};
pub use migration_sqlite::SqliteMigrator;

// Re-export helper types from document module
pub use document::{extract_filename_parts, sanitize_filename};

// Re-export DB record models
pub use models::{
    ConfigHistoryRecord, CrawlConfigRecord, CrawlRequestRecord, CrawlUrlRecord,
    DocumentPageRecord, DocumentRecord, DocumentVersionRecord, NewConfigHistory,
    NewCrawlRequest, NewCrawlUrl, NewDocument, NewDocumentPage, NewDocumentVersion,
    NewRateLimitState, NewScraperConfig, NewSource, NewVirtualFile, RateLimitStateRecord,
    ScraperConfigRecord, SourceRecord, VirtualFileRecord,
};

use chrono::{DateTime, Utc};

use self::diesel_context::DieselDbContext;

/// Bundled repository access for all database operations.
///
/// Provides bundled access to all repository types, eliminating repetitive
/// boilerplate in CLI commands.
pub struct Repositories {
    pub sources: DieselSourceRepository,
    pub crawl: DieselCrawlRepository,
    pub documents: DieselDocumentRepository,
    pub config_history: DieselConfigHistoryRepository,
    pub scraper_configs: DieselScraperConfigRepository,
    pub service_status: DieselServiceStatusRepository,
    pool: DbPool,
}

impl Repositories {
    pub fn new(ctx: DieselDbContext) -> Self {
        Self {
            sources: ctx.sources(),
            crawl: ctx.crawl(),
            documents: ctx.documents(),
            config_history: ctx.config_history(),
            scraper_configs: ctx.scraper_configs(),
            service_status: ctx.service_status(),
            pool: ctx.pool().clone(),
        }
    }

    pub fn pool(&self) -> &DbPool {
        &self.pool
    }

    pub async fn schema_version(&self) -> Result<Option<String>, DieselError> {
        DieselDbContext::with_pool(self.pool.clone())
            .get_schema_version()
            .await
    }
}

/// Parse a datetime string from the database, defaulting to Unix epoch on error.
pub fn parse_datetime(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(DateTime::UNIX_EPOCH)
}

/// Parse an optional datetime string from the database.
pub fn parse_datetime_opt(s: Option<String>) -> Option<DateTime<Utc>> {
    s.and_then(|s| {
        DateTime::parse_from_rfc3339(&s)
            .map(|dt| dt.with_timezone(&Utc))
            .ok()
    })
}
