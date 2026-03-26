//! Repository layer — re-exports from `foia-db`.

// Re-export all public modules from foia-db
pub use foia_db::{
    context, diesel_config_history, diesel_context, diesel_crawl, diesel_document,
    diesel_scraper_config, diesel_service_status, diesel_source, migration,
    migration_sqlite, models, pool, sea_tables, source, util,
};

#[cfg(feature = "postgres")]
pub use foia_db::{migration_postgres, pg_tls};

// Re-export migration runner (was repository/migrations.rs, now foia_db::migrate)
pub use foia_db::migrate as migrations;

// Re-export key types at this level for convenience
pub use foia_db::{
    parse_datetime, parse_datetime_opt,
    extract_filename_parts, sanitize_filename,
    DbContext, DbError, DbPool, DieselError,
    Repositories, SourceRepository,
    DieselConfigHistoryRepository, DieselCrawlRepository, DieselDocumentRepository,
    DieselScraperConfigRepository, DieselServiceStatusRepository, DieselSourceRepository,
    DatabaseExporter, DatabaseImporter, SqliteMigrator,
    ConfigHistoryRecord, CrawlConfigRecord, CrawlRequestRecord, CrawlUrlRecord,
    DocumentPageRecord, DocumentRecord, DocumentVersionRecord,
    NewConfigHistory, NewCrawlRequest, NewCrawlUrl, NewDocument,
    NewDocumentPage, NewDocumentVersion, NewRateLimitState, NewScraperConfig,
    NewSource, NewVirtualFile, RateLimitStateRecord, ScraperConfigRecord,
    SourceRecord, VirtualFileRecord,
};
